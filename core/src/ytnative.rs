// YouTube without yt-dlp, for Android (where yt-dlp can't run). Uses RustyPipe, a Rust
// client for YouTube's own website API in the spirit of NewPipe. Same results as the
// yt-dlp path: search, video/playlist links, and audio downloads that can play while
// they're still arriving.
use crate::app::App;
use crate::cache::Progress;
use crate::util::{ApiError, ApiResult};
use once_cell::sync::OnceCell;
use rustypipe::client::RustyPipe;
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
    /// so playback can start before it's finished.
    pub async fn ytn_download(&self, id: &str, dir: &Path, tx: watch::Sender<Progress>, on_progress: impl Fn(f64) + Send + Sync + 'static) -> ApiResult {
        let rp = self.rp()?;
        let player = rp.query().player(id).await.map_err(rp_err)?;
        if player.drm.is_some() { return Err(ApiError::new(422, "This video is DRM-protected")); }
        let mut streams = player.audio_streams.clone();
        if streams.is_empty() { return Err(ApiError::new(422, "No audio stream available for this video")); }
        // AAC (m4a) plays everywhere; among those, the highest bitrate.
        streams.sort_by_key(|s| (s.format != AudioFormat::M4a, std::cmp::Reverse(s.bitrate)));
        let s = &streams[0];
        let ext = if s.format == AudioFormat::M4a { "m4a" } else { "webm" };
        let name = format!("audio.{ext}");
        let mime = s.mime.split(';').next().unwrap_or("audio/mp4").trim().to_string();
        let total = s.size;
        tx.send_modify(|p| { p.kind = "file".into(); p.file = name.clone(); p.mime = mime.clone(); p.expected = if total > 0 { Some(total) } else { None }; });

        let mut f = tokio::fs::File::create(dir.join(&name)).await?;
        let mut written: u64 = 0;
        // YouTube throttles single long requests, so fetch in ranged chunks.
        const CHUNK: u64 = 4 * 1024 * 1024;
        loop {
            let end = if total > 0 { (written + CHUNK - 1).min(total - 1) } else { written + CHUNK - 1 };
            let res = self.http.get(&s.url).header("Range", format!("bytes={written}-{end}")).send().await?;
            let status = res.status().as_u16();
            if status == 416 && total == 0 { break; } // unknown size: past the end
            if !(200..300).contains(&status) { return Err(ApiError::new(502, format!("YouTube returned HTTP {status} for the audio stream"))); }
            let mut got = 0u64;
            let mut stream = res.bytes_stream();
            use futures::StreamExt;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk?;
                f.write_all(&chunk).await?;
                f.flush().await?;
                written += chunk.len() as u64;
                got += chunk.len() as u64;
                let w = written;
                tx.send_modify(|p| { p.written = w; if w >= crate::cache_start_bytes().min(p.expected.unwrap_or(u64::MAX)) { p.ready = true; } });
                if total > 0 { on_progress(written as f64 / total as f64); }
            }
            if got == 0 || (total > 0 && written >= total) || (total == 0 && got < CHUNK) { break; }
        }
        if total > 0 && written < total { return Err(ApiError::new(502, "Download was cut short")); }
        tx.send_modify(|p| { p.expected = Some(written); p.written = written; });
        Ok(json!({ "type": "file", "file": name, "mime": mime, "bytes": written }))
    }
}
