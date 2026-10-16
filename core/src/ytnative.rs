// YouTube without yt-dlp, for Android (where yt-dlp can't run). Uses RustyPipe, a Rust
// client for YouTube's own website API in the spirit of NewPipe. Same results as the
// yt-dlp path: search, video/playlist links, and audio downloads that can play while
// they're still arriving.
use crate::app::App;
use crate::cache::Progress;
use crate::util::{ApiError, ApiResult};
use once_cell::sync::OnceCell;
use rustypipe::client::{ClientType, RustyPipe};
use rustypipe::model::{AudioFormat, VideoItem};
use rustypipe::param::search_filter::{ItemType, SearchFilter};
use serde_json::{json, Value};
use std::path::Path;
use tokio::sync::watch;

static CLIENT: OnceCell<RustyPipe> = OnceCell::new();

fn rp_err(e: rustypipe::error::Error) -> ApiError {
    let m = e.to_string();
    crate::logf!("[youtube] {m}");
    ApiError::new(502, crate::youtube::friendly_error(&m))
}

fn thumb(id: &str) -> String { format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg") }

fn map_item(v: &VideoItem) -> Option<Value> {
    if !crate::youtube::valid_id(&v.id) || v.is_upcoming { return None; }
    Some(json!({
        "source": "youtube", "id": v.id,
        "title": if v.name.is_empty() { "(untitled)".to_string() } else { v.name.clone() },
        "artist": v.channel.as_ref().map(|c| c.name.clone()).unwrap_or_default(),
        "duration": v.duration.map(|d| d as f64 * 1000.0).unwrap_or(0.0),
        "artwork": thumb(&v.id),
        "url": format!("https://www.youtube.com/watch?v={}", v.id),
        "playable": if v.is_live { "unavailable" } else { "ok" },
    }))
}

impl App {
    fn rp(&self) -> ApiResult<&'static RustyPipe> {
        CLIENT.get_or_try_init(|| {
            RustyPipe::builder()
                .storage_dir(self.data_dir().join("youtube"))
                .build()
                .map_err(rp_err)
        })
    }

    pub async fn ytn_search(&self, q: &str, offset: u64) -> ApiResult {
        let rp = self.rp()?;
        let query = rp.query();
        let filter = SearchFilter::new().item_type(ItemType::Video);
        let res = query.search_filter::<VideoItem, _>(q, &filter).await.map_err(rp_err)?;
        let mut items = res.items;
        let want = (offset + 20) as usize;
        items.extend_limit(&query, want).await.map_err(rp_err)?;
        let out: Vec<Value> = items.items.iter().skip(offset as usize).take(20).filter_map(map_item)
            .filter(|t| { let d = t["duration"].as_f64().unwrap_or(0.0); d == 0.0 || d < 3.0 * 3600e3 })
            .collect();
        let more = items.items.len() >= want || items.ctoken.is_some();
        Ok(json!({ "items": out, "next": if more && want < 100 { json!(want) } else { Value::Null } }))
    }

    pub async fn ytn_resolve(&self, link: &str) -> ApiResult {
        let rp = self.rp()?;
        let query = rp.query();
        match crate::youtube::parse_url(link) {
            Some(crate::youtube::YtLink::Video(id)) => {
                let p = query.player(&id).await.map_err(rp_err)?;
                let d = &p.details;
                Ok(json!({ "kind": "track", "tracks": [{
                    "source": "youtube", "id": d.id, "title": d.name.clone().unwrap_or_else(|| "(untitled)".into()),
                    "artist": d.channel_name.clone().unwrap_or_default(), "duration": d.duration as f64 * 1000.0,
                    "artwork": thumb(&d.id), "url": format!("https://www.youtube.com/watch?v={}", d.id),
                    "playable": if d.is_live { "unavailable" } else { "ok" },
                }] }))
            }
            Some(crate::youtube::YtLink::Playlist(list)) => {
                let mut pl = query.playlist(&list).await.map_err(rp_err)?;
                pl.videos.extend_limit(&query, 500).await.map_err(rp_err)?;
                let tracks: Vec<Value> = pl.videos.items.iter().filter_map(map_item).collect();
                if tracks.is_empty() { return Err(ApiError::bad("That playlist is empty or private")); }
                Ok(json!({ "kind": "playlist", "name": pl.name, "tracks": tracks }))
            }
            None => Err(ApiError::bad("Paste a YouTube video or playlist link")),
        }
    }

    /// Download a video's audio into dir, reporting progress so playback can start before
    /// it's finished. First NewPipe's current approach (the visionOS client, see
    /// visionos.rs); then RustyPipe's clients, each fetched with that client's user agent.
    /// A client is only abandoned while nothing has reached the player yet.
    pub async fn ytn_download(&self, id: &str, dir: &Path, tx: watch::Sender<Progress>, on_progress: impl Fn(f64) + Send + Sync + 'static) -> ApiResult {
        match self.vos_download(id, dir, &tx, &on_progress).await {
            Ok(v) => return Ok(v),
            Err((e, written)) => {
                crate::logf!("[youtube] visionOS client failed after {written} bytes: {}", e.message);
                if written > 0 || e.status.as_u16() == 422 { return Err(e); }
            }
        }
        let rp = self.rp()?;
        let q = rp.query();
        let order: Vec<ClientType> = q.player_client_order().to_vec();
        let mut last: Option<ApiError> = None;
        for i in 0..order.len().max(1) {
            let mut clients = order[i..].to_vec();
            clients.extend_from_slice(&order[..i]);
            let player = match q.player_from_clients(id, &clients).await {
                Ok(p) => p,
                Err(e) => { last = Some(rp_err(e)); continue; }
            };
            if player.drm.is_some() { return Err(ApiError::new(422, "This video is DRM-protected")); }
            let mut streams = player.audio_streams.clone();
            if streams.is_empty() { last = Some(ApiError::new(422, "No audio stream available for this video")); continue; }
            // AAC (m4a) plays everywhere; among those, the highest bitrate.
            streams.sort_by_key(|s| (s.format != AudioFormat::M4a, std::cmp::Reverse(s.bitrate)));
            let st = &streams[0];
            let ext = if st.format == AudioFormat::M4a { "m4a" } else { "webm" };
            let mime = st.mime.split(';').next().unwrap_or("audio/mp4").trim().to_string();
            let ua = q.user_agent(player.client_type).to_string();
            match self.fetch_stream(&st.url, &ua, st.size, &format!("audio.{ext}"), &mime, dir, &tx, &on_progress).await {
                Ok(v) => return Ok(v),
                Err((e, written)) => {
                    crate::logf!("[youtube] {:?} client failed after {} bytes: {}", player.client_type, written, e.message);
                    if written > 0 { return Err(e); }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| ApiError::new(502, "YouTube didn't return a playable stream")))
    }
}
