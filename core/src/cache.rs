// Downloads SoundCloud and YouTube audio and keeps it on disk.
//
// Songs start playing while they download: as soon as the first part is on disk the
// player gets a URL, and the server feeds it the rest as it arrives.
//   * HLS (most SoundCloud tracks): the local playlist is written first; each segment
//     request waits until that segment has been downloaded.
//   * Single files (YouTube, some SoundCloud): the file grows on disk and requests for it
//     are streamed, waiting for bytes that haven't arrived yet.
// The next song in the queue is fetched the same way well before it's needed, so it's
// normally complete by the time it crossfades in.
//
// <data>/cache/<key>/      key = SoundCloud track id (digits) or yt_<YouTube video id>
//   meta.json              { type: "hls" | "file", file, mime, preview, bytes, complete }
//   index.m3u8, init.mp4, s0.m4s…  (hls)   or   audio.<ext>  (file)
use crate::app::App;
use crate::util::{s, ApiError, ApiResult};
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

/// Enough of a single-file download to start playing.
const START_BYTES: u64 = 96 * 1024;

/// Live state of a download in progress.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub kind: String,        // "hls" | "file" ("" until known)
    pub file: String,        // file the player should load
    pub mime: String,
    pub preview: bool,
    pub expected: Option<u64>, // final size of a single-file download, when known
    pub written: u64,          // bytes of `file` safely on disk
    pub segments_total: usize,
    pub segments_done: usize,
    pub ready: bool, // playable from the start
    pub done: bool,
    pub error: Option<String>,
}

pub struct CacheState {
    pub dir: PathBuf,
    pub live: Mutex<HashMap<String, watch::Sender<Progress>>>,
    pub status: Mutex<HashMap<String, Value>>,
}

impl CacheState {
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("cache");
        std::fs::create_dir_all(&dir).ok();
        CacheState { dir, live: Mutex::new(HashMap::new()), status: Mutex::new(HashMap::new()) }
    }
}

pub fn cache_key(source: &str, id: &str) -> ApiResult<String> {
    if source == "soundcloud" && !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) { return Ok(id.to_string()); }
    if source == "youtube" && crate::youtube::valid_id(id) { return Ok(format!("yt_{id}")); }
    Err(ApiError::bad("bad track id"))
}

pub fn valid_key(key: &str) -> bool {
    (!key.is_empty() && key.chars().all(|c| c.is_ascii_digit())) || key.strip_prefix("yt_").map(crate::youtube::valid_id).unwrap_or(false)
}

fn ext_of(u: &str, fallback: &str) -> String {
    url::Url::parse(u).ok()
        .and_then(|u| u.path().rsplit('/').next().map(|s| s.to_string()))
        .and_then(|name| name.rsplit_once('.').map(|(_, e)| e.to_lowercase()))
        .filter(|e| (2..=4).contains(&e.len()) && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or_else(|| fallback.to_string())
}

/// What /api/prepare returns.
pub fn prepare_response(key: &str, meta: &Value) -> Value {
    json!({
        "type": meta["type"], "src": format!("/media/{key}/{}", s(&meta["file"])), "mime": meta["mime"],
        "preview": meta["preview"] == json!(true), "partial": meta["partial"] == json!(true),
    })
}

impl App {
    pub fn cache_dir_for(&self, key: &str) -> ApiResult<PathBuf> {
        if !valid_key(key) { return Err(ApiError::bad("bad track id")); }
        Ok(self.cache.dir.join(key))
    }

    async fn cache_meta(&self, key: &str) -> Option<Value> {
        let p = self.cache_dir_for(key).ok()?.join("meta.json");
        let m: Value = serde_json::from_str(&tokio::fs::read_to_string(&p).await.ok()?).ok()?;
        if m["complete"] == json!(true) { Some(m) } else { None }
    }

    fn set_status(&self, key: &str, v: Value) { self.cache.status.lock().insert(key.to_string(), v); }

    /// Live progress of a download that's still running, if any.
    pub fn cache_live(&self, key: &str) -> Option<watch::Receiver<Progress>> {
        self.cache.live.lock().get(key).map(|tx| tx.subscribe())
    }

    /// Return as soon as the track can start playing (fully downloaded, or far enough
    /// along to stream). The download carries on in the background.
    pub async fn cache_prepare(self: &Arc<Self>, key: &str) -> ApiResult {
        let dir = self.cache_dir_for(key)?;
        if self.cache_live(key).is_none() {
            if let Some(mut m) = self.cache_meta(key).await {
                // Mark as recently used for the size limit.
                if let Ok(f) = std::fs::OpenOptions::new().append(true).open(dir.join("meta.json")) { let _ = f.set_modified(std::time::SystemTime::now()); }
                self.set_status(key, json!({ "state": "ready", "progress": 1 }));
                m["partial"] = json!(false);
                return Ok(m);
            }
        }
        let mut rx = self.cache_start(key);
        loop {
            {
                let p = rx.borrow_and_update().clone();
                if let Some(e) = p.error { return Err(ApiError::new(422, e)); }
                if p.ready || p.done {
                    return Ok(json!({ "type": p.kind, "file": p.file, "mime": p.mime, "preview": p.preview, "partial": !p.done }));
                }
            }
            if rx.changed().await.is_err() {
                // The download finished and its channel closed between checks.
                return match self.cache_meta(key).await {
                    Some(mut m) => { m["partial"] = json!(false); Ok(m) }
                    None => Err(ApiError::new(502, "Download stopped unexpectedly")),
                };
            }
        }
    }

    /// Start downloading `key` unless it's already running; returns its progress feed.
    fn cache_start(self: &Arc<Self>, key: &str) -> watch::Receiver<Progress> {
        let mut live = self.cache.live.lock();
        if let Some(tx) = live.get(key) { return tx.subscribe(); }
        let (tx, rx) = watch::channel(Progress::default());
        live.insert(key.to_string(), tx.clone());
        drop(live);
        let app = self.clone();
        let k = key.to_string();
        self.set_status(key, json!({ "state": "downloading", "progress": 0 }));
        tokio::spawn(async move {
            match app.cache_download(&k, &tx).await {
                Ok(()) => {
                    tx.send_modify(|p| { p.done = true; p.ready = true; });
                    app.set_status(&k, json!({ "state": "ready", "progress": 1 }));
                }
                Err(e) => {
                    crate::logf!("[cache] {k}: {}", e.message);
                    tx.send_modify(|p| p.error = Some(e.message.clone()));
                    app.set_status(&k, json!({ "state": "error", "error": e.message }));
                    if let Ok(d) = app.cache_dir_for(&k) { let _ = tokio::fs::remove_dir_all(d).await; }
                }
            }
            app.cache.live.lock().remove(&k);
            let a2 = app.clone();
            tokio::spawn(async move { a2.cache_prune().await; });
        });
        rx
    }

    async fn cache_download(self: &Arc<Self>, key: &str, tx: &watch::Sender<Progress>) -> ApiResult<()> {
        let dir = self.cache_dir_for(key)?;
        let meta = if let Some(vid) = key.strip_prefix("yt_") {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await?;
            let app = self.clone();
            let k2 = key.to_string();
            self.yt_download(vid, &dir, tx.clone(), move |p| app.set_status(&k2, json!({ "state": "downloading", "progress": p }))).await?
        } else {
            let (protocol, mime, url, preview) = self.sc_stream_info(key).await?;
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await?;
            tx.send_modify(|p| p.preview = preview);
            let mut m = if protocol == "hls" { self.cache_hls(key, &dir, &url, tx).await? } else { self.cache_file(key, &dir, &url, &mime, tx).await? };
            m["preview"] = json!(preview);
            m
        };
        let mut meta = meta;
        meta["complete"] = json!(true);
        tokio::fs::write(dir.join("meta.json"), meta.to_string()).await?;
        Ok(())
    }

    /// Single file: stream it to disk, telling listeners how much has arrived.
    async fn cache_file(&self, key: &str, dir: &Path, url: &str, mime: &str, tx: &watch::Sender<Progress>) -> ApiResult {
        let res = self.sc_download(url).await?;
        let expected = res.content_length();
        let ext = if mime.contains("mpeg") || mime.contains("mp3") { "mp3".to_string() } else { ext_of(url, "mp3") };
        let name = format!("audio.{ext}");
        let mime = if mime.is_empty() { "audio/mpeg".to_string() } else { mime.to_string() };
        let mut f = tokio::fs::File::create(dir.join(&name)).await?;
        tx.send_modify(|p| { p.kind = "file".into(); p.file = name.clone(); p.mime = mime.clone(); p.expected = expected; });
        let mut written = 0u64;
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            f.write_all(&chunk).await?;
            f.flush().await?;
            written += chunk.len() as u64;
            let ready = written >= START_BYTES.min(expected.unwrap_or(u64::MAX));
            tx.send_modify(|p| { p.written = written; if ready { p.ready = true; } });
            if let Some(total) = expected { self.set_status(key, json!({ "state": "downloading", "progress": written as f64 / total.max(1) as f64 })); }
        }
        if let Some(total) = expected {
            if written < total { return Err(ApiError::new(502, "Download was cut short")); }
        }
        Ok(json!({ "type": "file", "file": name, "mime": mime, "bytes": written }))
    }

    /// HLS: write the local playlist first, then fetch segments (first ones first).
    async fn cache_hls(&self, key: &str, dir: &Path, url: &str, tx: &watch::Sender<Progress>) -> ApiResult {
        let mut text = self.sc_fetch_text(url).await?;
        let mut base = url::Url::parse(url).map_err(|_| ApiError::bad("bad stream url"))?;
        // Master playlist? Follow the first variant.
        if text.contains("#EXT-X-STREAM-INF") {
            let variant = text.lines().find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or("").to_string();
            base = base.join(&variant).map_err(|_| ApiError::bad("bad variant url"))?;
            text = self.sc_fetch_text(base.as_str()).await?;
        }
        if let Some(line) = text.lines().find(|l| l.starts_with("#EXT-X-KEY:")) {
            if !line.contains("METHOD=NONE") { return Err(ApiError::new(422, "Encrypted stream, skipping")); }
        }
        let re_uri = regex::Regex::new(r#"URI="([^"]+)""#).unwrap();
        let mut init: Option<(String, String)> = None;
        let mut segments: Vec<(String, String)> = Vec::new();
        let mut out: Vec<String> = Vec::new();
        for line in text.lines() {
            if line.starts_with("#EXT-X-MAP:") {
                if let Some(c) = re_uri.captures(line) {
                    let abs = base.join(&c[1]).map(|u| u.to_string()).unwrap_or_default();
                    let name = format!("init.{}", ext_of(&abs, "mp4"));
                    out.push(line.replace(&c[0], &format!("URI=\"{name}\"")));
                    init = Some((abs, name));
                    continue;
                }
                out.push(line.to_string());
            } else if !line.is_empty() && !line.starts_with('#') {
                let abs = base.join(line).map(|u| u.to_string()).unwrap_or_default();
                let name = format!("s{}.{}", segments.len(), ext_of(&abs, "ts"));
                out.push(name.clone());
                segments.push((abs, name));
            } else {
                out.push(line.to_string());
            }
        }
        if !text.contains("#EXT-X-ENDLIST") { out.push("#EXT-X-ENDLIST".into()); }
        let is_mp4 = init.is_some() || segments.iter().any(|(_, n)| n.ends_with(".m4s") || n.ends_with(".mp4") || n.ends_with(".m4a"));
        let mime = if is_mp4 { "audio/mp4" } else { "audio/mpeg" };
        let total = segments.len();
        tokio::fs::write(dir.join("index.m3u8"), out.join("\n")).await?;
        tx.send_modify(|p| { p.kind = "hls".into(); p.file = "index.m3u8".into(); p.mime = mime.into(); p.segments_total = total; });

        let bytes = std::sync::atomic::AtomicUsize::new(0);
        if let Some((u, name)) = &init {
            let n = self.fetch_segment(u, &dir.join(name)).await?;
            bytes.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
        }
        let done = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<ApiResult<()>> = futures::stream::iter(segments.iter().cloned().enumerate())
            .map(|(i, (u, name))| {
                let done = &done;
                let bytes = &bytes;
                async move {
                    let n = self.fetch_segment(&u, &dir.join(&name)).await?;
                    bytes.fetch_add(n, std::sync::atomic::Ordering::SeqCst);
                    let d = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    // Playable once the opening segment is on disk.
                    tx.send_modify(|p| { p.segments_done = d; if i == 0 { p.ready = true; } });
                    self.set_status(key, json!({ "state": "downloading", "progress": d as f64 / total.max(1) as f64 }));
                    Ok(())
                }
            })
            .buffered(4) // keep request order so the opening segments arrive first
            .collect()
            .await;
        for r in results { r?; }
        Ok(json!({ "type": "hls", "file": "index.m3u8", "mime": mime, "bytes": bytes.load(std::sync::atomic::Ordering::SeqCst), "segments": total }))
    }

    /// Download one segment, writing it atomically so readers never see half a file.
    async fn fetch_segment(&self, url: &str, dest: &Path) -> ApiResult<usize> {
        let mut last: Option<ApiError> = None;
        for attempt in 0..3u64 {
            match async { Ok::<_, ApiError>(self.sc_download(url).await?.bytes().await?) }.await {
                Ok(b) => {
                    let tmp = dest.with_extension("part");
                    tokio::fs::write(&tmp, &b).await?;
                    tokio::fs::rename(&tmp, dest).await?;
                    return Ok(b.len());
                }
                Err(e) => { last = Some(e); tokio::time::sleep(std::time::Duration::from_millis(500 * (attempt + 1))).await; }
            }
        }
        Err(last.unwrap_or_else(|| ApiError::new(502, "segment download failed")))
    }

    fn active_keys(&self) -> Vec<String> { self.cache.live.lock().keys().cloned().collect() }

    /// Keep the cache under the size limit, dropping least-recently-used tracks.
    pub async fn cache_prune(&self) {
        let limit = self.config.lock().data.settings.cache_mb * 1024 * 1024;
        let active = self.active_keys();
        let mut entries: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
        let Ok(mut rd) = tokio::fs::read_dir(&self.cache.dir).await else { return };
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            if active.contains(&name) { continue; }
            let meta_path = e.path().join("meta.json");
            let parsed = async {
                let st = tokio::fs::metadata(&meta_path).await.ok()?;
                let m: Value = serde_json::from_str(&tokio::fs::read_to_string(&meta_path).await.ok()?).ok()?;
                if m["complete"] != json!(true) { return None; }
                Some((st.modified().ok()?, m["bytes"].as_u64().unwrap_or(0)))
            }.await;
            match parsed {
                Some((t, b)) => entries.push((e.path(), t, b)),
                None => { let _ = tokio::fs::remove_dir_all(e.path()).await; } // incomplete leftovers
            }
        }
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        let mut total = 0u64;
        for (p, _, b) in entries {
            total += b;
            if total > limit { let _ = tokio::fs::remove_dir_all(p).await; }
        }
    }

    pub async fn cache_clear(&self) {
        let active = self.active_keys();
        if let Ok(mut rd) = tokio::fs::read_dir(&self.cache.dir).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if !active.contains(&e.file_name().to_string_lossy().to_string()) { let _ = tokio::fs::remove_dir_all(e.path()).await; }
            }
        }
    }

    pub async fn cache_size(&self) -> u64 {
        let mut total = 0;
        if let Ok(mut rd) = tokio::fs::read_dir(&self.cache.dir).await {
            while let Ok(Some(e)) = rd.next_entry().await {
                if let Ok(t) = tokio::fs::read_to_string(e.path().join("meta.json")).await {
                    total += serde_json::from_str::<Value>(&t).ok().and_then(|m| m["bytes"].as_u64()).unwrap_or(0);
                }
            }
        }
        total
    }
}
