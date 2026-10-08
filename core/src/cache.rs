// Downloads SoundCloud and YouTube audio ahead of time and keeps it on disk, so the player
// can load the next track instantly and crossfade without network hiccups.
//
// <data>/cache/<key>/      key = SoundCloud track id (digits) or yt_<YouTube video id>
//   meta.json              { type: "hls" | "file", file, mime, preview, bytes, complete }
//   index.m3u8, init.mp4, s0.m4s…  (hls)   or   audio.<ext>  (file)
use crate::app::App;
use crate::util::{s, ApiError, ApiResult};
use futures::future::{BoxFuture, FutureExt, Shared};
use futures::StreamExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

type Job = Shared<BoxFuture<'static, Result<Value, (u16, String)>>>;

pub struct CacheState {
    pub dir: PathBuf,
    jobs: Mutex<HashMap<String, Job>>,
    pub status: Mutex<HashMap<String, Value>>,
}

impl CacheState {
    pub fn new(data_dir: &Path) -> Self {
        let dir = data_dir.join("cache");
        std::fs::create_dir_all(&dir).ok();
        CacheState { dir, jobs: Mutex::new(HashMap::new()), status: Mutex::new(HashMap::new()) }
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

    /// Make sure the track is fully downloaded. Concurrent callers share one job.
    pub async fn cache_prepare(self: &Arc<Self>, key: &str) -> ApiResult {
        let job = {
            let mut jobs = self.cache.jobs.lock();
            if let Some(j) = jobs.get(key) { j.clone() } else {
                let app = self.clone();
                let k = key.to_string();
                let fut: BoxFuture<'static, Result<Value, (u16, String)>> = async move {
                    let r = app.cache_prepare_inner(&k).await;
                    app.cache.jobs.lock().remove(&k);
                    match r {
                        Ok(v) => Ok(v),
                        Err(e) => {
                            app.set_status(&k, json!({ "state": "error", "error": e.message }));
                            Err((e.status.as_u16(), e.message))
                        }
                    }
                }.boxed();
                let shared = fut.shared();
                jobs.insert(key.to_string(), shared.clone());
                shared
            }
        };
        job.await.map_err(|(st, msg)| ApiError::new(st, msg))
    }

    async fn cache_prepare_inner(self: &Arc<Self>, key: &str) -> ApiResult {
        if let Some(m) = self.cache_meta(key).await {
            // Mark as recently used for the size limit.
            let p = self.cache_dir_for(key)?.join("meta.json");
            if let Ok(f) = std::fs::OpenOptions::new().append(true).open(&p) { let _ = f.set_modified(std::time::SystemTime::now()); }
            self.set_status(key, json!({ "state": "ready", "progress": 1 }));
            return Ok(m);
        }
        self.set_status(key, json!({ "state": "downloading", "progress": 0 }));
        let dir = self.cache_dir_for(key)?;
        let mut meta = if let Some(vid) = key.strip_prefix("yt_") {
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await?;
            let app = self.clone();
            let k2 = key.to_string();
            self.yt_download(vid, &dir, move |p| app.set_status(&k2, json!({ "state": "downloading", "progress": p }))).await?
        } else {
            let (protocol, mime, url, preview) = self.sc_stream_info(key).await?;
            let _ = tokio::fs::remove_dir_all(&dir).await;
            tokio::fs::create_dir_all(&dir).await?;
            let mut m = if protocol == "hls" { self.cache_hls(key, &dir, &url).await? } else { self.cache_file(&dir, &url, &mime).await? };
            m["preview"] = json!(preview);
            m
        };
        meta["complete"] = json!(true);
        tokio::fs::write(dir.join("meta.json"), meta.to_string()).await?;
        self.set_status(key, json!({ "state": "ready", "progress": 1 }));
        let app = self.clone();
        tokio::spawn(async move { app.cache_prune().await; });
        Ok(meta)
    }

    async fn cache_file(&self, dir: &Path, url: &str, mime: &str) -> ApiResult {
        let bytes = self.sc_download(url).await?.bytes().await?;
        let ext = if mime.contains("mpeg") || mime.contains("mp3") { "mp3".to_string() } else { ext_of(url, "mp3") };
        let name = format!("audio.{ext}");
        tokio::fs::write(dir.join(&name), &bytes).await?;
        Ok(json!({ "type": "file", "file": name, "mime": if mime.is_empty() { "audio/mpeg" } else { mime }, "bytes": bytes.len() }))
    }

    async fn cache_hls(&self, key: &str, dir: &Path, url: &str) -> ApiResult {
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
        let mut downloads: Vec<(String, String)> = Vec::new();
        let mut out: Vec<String> = Vec::new();
        let mut n = 0;
        for line in text.lines() {
            if line.starts_with("#EXT-X-MAP:") {
                if let Some(c) = re_uri.captures(line) {
                    let abs = base.join(&c[1]).map(|u| u.to_string()).unwrap_or_default();
                    let name = format!("init.{}", ext_of(&abs, "mp4"));
                    out.push(line.replace(&c[0], &format!("URI=\"{name}\"")));
                    downloads.push((abs, name));
                    continue;
                }
                out.push(line.to_string());
            } else if !line.is_empty() && !line.starts_with('#') {
                let abs = base.join(line).map(|u| u.to_string()).unwrap_or_default();
                let name = format!("s{n}.{}", ext_of(&abs, "ts"));
                n += 1;
                out.push(name.clone());
                downloads.push((abs, name));
            } else {
                out.push(line.to_string());
            }
        }
        if !text.contains("#EXT-X-ENDLIST") { out.push("#EXT-X-ENDLIST".into()); }

        let total = downloads.len().max(1);
        let done = std::sync::atomic::AtomicUsize::new(0);
        let bytes = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<ApiResult<()>> = futures::stream::iter(downloads.iter().cloned())
            .map(|(u, name)| {
                let done = &done;
                let bytes = &bytes;
                async move {
                    let mut last: Option<ApiError> = None;
                    for attempt in 0..3u64 {
                        match async { Ok::<_, ApiError>(self.sc_download(&u).await?.bytes().await?) }.await {
                            Ok(b) => {
                                tokio::fs::write(dir.join(&name), &b).await?;
                                bytes.fetch_add(b.len(), std::sync::atomic::Ordering::SeqCst);
                                last = None;
                                break;
                            }
                            Err(e) => { last = Some(e); tokio::time::sleep(std::time::Duration::from_millis(500 * (attempt + 1))).await; }
                        }
                    }
                    if let Some(e) = last { return Err(e); }
                    let d = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    self.set_status(key, json!({ "state": "downloading", "progress": d as f64 / total as f64 }));
                    Ok(())
                }
            })
            .buffer_unordered(4)
            .collect()
            .await;
        for r in results { r?; }
        tokio::fs::write(dir.join("index.m3u8"), out.join("\n")).await?;
        let mime = if downloads.iter().any(|(_, n)| n.ends_with(".m4s") || n.ends_with(".mp4") || n.ends_with(".m4a")) { "audio/mp4" } else { "audio/mpeg" };
        Ok(json!({ "type": "hls", "file": "index.m3u8", "mime": mime, "bytes": bytes.load(std::sync::atomic::Ordering::SeqCst), "segments": n }))
    }

    /// Keep the cache under the size limit, dropping least-recently-used tracks.
    pub async fn cache_prune(&self) {
        let limit = self.config.lock().data.settings.cache_mb * 1024 * 1024;
        let active: Vec<String> = self.cache.jobs.lock().keys().cloned().collect();
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
        let active: Vec<String> = self.cache.jobs.lock().keys().cloned().collect();
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

/// Response body for POST /api/prepare.
pub fn prepare_response(key: &str, meta: &Value) -> Value {
    json!({ "type": meta["type"], "src": format!("/media/{key}/{}", s(&meta["file"])), "mime": meta["mime"], "preview": meta["preview"] == json!(true) })
}
