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
use tokio::io::AsyncWriteExt;
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

    /// Download the best audio-only stream (AAC preferred) into dir, reporting progress
    /// so playback can start before it's finished. Stream links only work when fetched
    /// with the same app identity YouTube issued them to, so each attempt uses that
    /// client's user agent; if YouTube refuses one client, the next one is tried.
    pub async fn ytn_download(&self, id: &str, dir: &Path, tx: watch::Sender<Progress>, on_progress: impl Fn(f64) + Send + Sync + 'static) -> ApiResult {
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
            let ua = q.user_agent(player.client_type).to_string();
            match self.ytn_fetch(&player, &ua, dir, &tx, &on_progress).await {
                Ok(v) => return Ok(v),
                Err((e, written)) => {
                    crate::logf!("[youtube] {:?} stream failed after {} bytes: {}", player.client_type, written, e.message);
                    // Once audio has been handed to the player, switching streams would corrupt it.
                    if written > 0 { return Err(e); }
                    last = Some(e);
                }
            }
        }
        Err(last.unwrap_or_else(|| ApiError::new(502, "YouTube didn't return a playable stream")))
    }

    async fn ytn_fetch(&self, player: &rustypipe::model::VideoPlayer, ua: &str, dir: &Path, tx: &watch::Sender<Progress>,
                       on_progress: &(impl Fn(f64) + Send + Sync)) -> Result<Value, (ApiError, u64)> {
        let mut streams = player.audio_streams.clone();
        if streams.is_empty() { return Err((ApiError::new(422, "No audio stream available for this video"), 0)); }
        // AAC (m4a) plays everywhere; among those, the highest bitrate.
        streams.sort_by_key(|s| (s.format != AudioFormat::M4a, std::cmp::Reverse(s.bitrate)));
        let s = &streams[0];
        let ext = if s.format == AudioFormat::M4a { "m4a" } else { "webm" };
        let name = format!("audio.{ext}");
        let mime = s.mime.split(';').next().unwrap_or("audio/mp4").trim().to_string();
        let total = s.size;
        tx.send_modify(|p| { p.kind = "file".into(); p.file = name.clone(); p.mime = mime.clone(); p.expected = if total > 0 { Some(total) } else { None }; p.written = 0; p.ready = false; });

        let mut f = tokio::fs::File::create(dir.join(&name)).await.map_err(|e| (ApiError::from(e), 0))?;
        let mut written: u64 = 0;
        // googlevideo links take the byte range as a URL parameter (a Range header gets
        // throttled). Start small so playback begins quickly, then fetch bigger pieces.
        let by_param = total > 0 && s.url.contains("googlevideo.com/videoplayback");
        let mut chunk: u64 = 512 * 1024;
        loop {
            let end = if total > 0 { (written + chunk - 1).min(total - 1) } else { written + chunk - 1 };
            let mut req = if by_param {
                self.http.get(format!("{}&range={written}-{end}", s.url))
            } else {
                self.http.get(&s.url).header("Range", format!("bytes={written}-{end}"))
            };
            req = req.header("User-Agent", ua)
                .header("Origin", "https://www.youtube.com")
                .header("Referer", "https://www.youtube.com/")
                .timeout(std::time::Duration::from_secs(60));
            let res = req.send().await.map_err(|e| (ApiError::from(e), written))?;
            let status = res.status().as_u16();
            if status == 416 && total == 0 { break; } // unknown size: past the end
            if !(200..300).contains(&status) {
                return Err((ApiError::new(502, format!("YouTube refused the audio stream (HTTP {status})")), written));
            }
            let mut got = 0u64;
            let mut stream = res.bytes_stream();
            use futures::StreamExt;
            while let Some(c) = stream.next().await {
                let c = c.map_err(|e| (ApiError::from(e), written))?;
                f.write_all(&c).await.map_err(|e| (ApiError::from(e), written))?;
                f.flush().await.map_err(|e| (ApiError::from(e), written))?;
                written += c.len() as u64;
                got += c.len() as u64;
                let w = written;
                tx.send_modify(|p| { p.written = w; if w >= crate::cache_start_bytes().min(p.expected.unwrap_or(u64::MAX)) { p.ready = true; } });
                if total > 0 { on_progress(written as f64 / total as f64); }
            }
            if got == 0 {
                if total > 0 && written < total { return Err((ApiError::new(502, "YouTube sent an empty piece of the audio"), written)); }
                break;
            }
            if (total > 0 && written >= total) || (total == 0 && got < chunk) { break; }
            chunk = (chunk * 2).min(8 * 1024 * 1024);
        }
        if total > 0 && written < total { return Err((ApiError::new(502, "Download was cut short"), written)); }
        tx.send_modify(|p| { p.expected = Some(written); p.written = written; });
        Ok(json!({ "type": "file", "file": name, "mime": mime, "bytes": written }))
    }
}
