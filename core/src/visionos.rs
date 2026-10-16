// YouTube audio through the YouTube visionOS (Apple Vision Pro) app's API: the workaround
// NewPipe adopted in 2026 after YouTube started enforcing its SABR streaming protocol and
// broke the Android/iOS clients (NewPipeExtractor 0.26.3+, "Workaround SABR enforcement by
// using another player client", and "Remove usage of ANDROID, IOS and WEB_EMBEDDED_PLAYER
// clients"). Like the iOS app, its stream links don't need YouTube's JavaScript player to
// unscramble them, so no JS engine is needed. Used on every platform: first choice on
// Android, fallback for yt-dlp on Windows/Linux.
use crate::app::App;
use crate::cache::Progress;
use crate::util::{s, ApiError, ApiResult};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rand::Rng;
use serde_json::{json, Value};
use std::path::Path;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

// Mirrors NewPipeExtractor's ClientsConstants (VISIONOS_*).
const CLIENT_NAME: &str = "VISIONOS";
const CLIENT_VERSION: &str = "1.04";
const DEVICE_MODEL: &str = "RealityDevice17,1";
const OS_VERSION: &str = "26.6.0.23O770";
const UA_OS_VERSION: &str = "26_6_0";
fn api() -> String { std::env::var("YT_VISIONOS_API").unwrap_or_else(|_| "https://youtubei.googleapis.com/youtubei/v1/".into()) }
const NONCE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// visitorData from YouTube, reused for a while (it's required for valid player responses).
static VISITOR: Lazy<Mutex<Option<(String, i64)>>> = Lazy::new(|| Mutex::new(None));

pub fn user_agent() -> String {
    format!("com.google.visionos.youtube/{CLIENT_VERSION}({DEVICE_MODEL}; U; CPU visionOS {UA_OS_VERSION} like Mac OS X; US)")
}

fn nonce(len: usize) -> String {
    let mut r = rand::thread_rng();
    (0..len).map(|_| NONCE_ALPHABET[r.gen_range(0..NONCE_ALPHABET.len())] as char).collect()
}

fn context(visitor: Option<&str>) -> Value {
    let mut client = json!({
        "clientName": CLIENT_NAME, "clientVersion": CLIENT_VERSION, "clientScreen": "WATCH",
        "platform": "MOBILE", "deviceMake": "Apple", "deviceModel": DEVICE_MODEL,
        "osName": "visionOS", "osVersion": OS_VERSION, "hl": "en", "gl": "US", "utcOffsetMinutes": 0,
    });
    if let Some(v) = visitor { client["visitorData"] = json!(v); }
    json!({
        "client": client,
        "request": { "internalExperimentFlags": [], "useSsl": true },
        "user": { "lockedSafetyMode": false },
    })
}

/// An audio stream picked from the player response.
pub struct VosStream {
    pub url: String,
    pub mime: String,
    pub ext: &'static str,
    pub size: u64,
}

impl App {
    async fn vos_post(&self, url: &str, body: &Value) -> ApiResult {
        let res = self.http.post(url)
            .header("User-Agent", user_agent())
            .header("X-Goog-Api-Format-Version", "2")
            .header("Content-Type", "application/json")
            .json(body)
            .timeout(std::time::Duration::from_secs(20))
            .send().await
            .map_err(|_| ApiError::new(502, "Couldn't reach YouTube"))?;
        let status = res.status().as_u16();
        let j: Value = res.json().await.unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            let msg = j["error"]["message"].as_str().unwrap_or("").to_string();
            return Err(ApiError::new(502, format!("YouTube returned HTTP {status} {msg}").trim().to_string()));
        }
        Ok(j)
    }

    async fn vos_visitor(&self) -> ApiResult<String> {
        if let Some((v, at)) = VISITOR.lock().clone() {
            if crate::util::now_ms() - at < 6 * 3600 * 1000 { return Ok(v); }
        }
        let j = self.vos_post(&format!("{}visitor_id?prettyPrint=false", api()), &json!({ "context": context(None) })).await?;
        let v = s(&j["responseContext"]["visitorData"]);
        if v.is_empty() { return Err(ApiError::new(502, "YouTube didn't return visitor data")); }
        *VISITOR.lock() = Some((v.clone(), crate::util::now_ms()));
        Ok(v)
    }

    /// Ask YouTube (as the visionOS app) for a video's audio streams and pick the best one:
    /// AAC (plays everywhere) at the highest bitrate, otherwise Opus.
    pub async fn vos_audio(&self, id: &str) -> ApiResult<VosStream> {
        let visitor = self.vos_visitor().await?;
        let cpn = nonce(16);
        let body = json!({
            "context": context(Some(&visitor)),
            "videoId": id, "cpn": cpn, "contentCheckOk": true, "racyCheckOk": true,
        });
        let url = format!("{}player?prettyPrint=false&t={}&id={id}", api(), nonce(12));
        let j = match self.vos_post(&url, &body).await {
            Ok(j) => j,
            Err(e) => { *VISITOR.lock() = None; return Err(e); }
        };
        let ps = &j["playabilityStatus"];
        let status = s(&ps["status"]);
        if status != "OK" {
            let reason = ps["reason"].as_str().or(ps["errorScreen"]["playerErrorMessageRenderer"]["reason"]["simpleText"].as_str()).unwrap_or(&status).to_string();
            return Err(ApiError::new(422, crate::youtube::friendly_error(&reason)));
        }
        let formats = j["streamingData"]["adaptiveFormats"].as_array().cloned().unwrap_or_default();
        let mut best: Option<(bool, u64, VosStream)> = None;
        for f in formats {
            let mime_full = s(&f["mimeType"]);
            if !mime_full.starts_with("audio/") { continue; }
            // Links that need YouTube's JavaScript to unscramble aren't usable here.
            let Some(u) = f["url"].as_str() else { continue };
            if f["drmFamilies"].is_array() { continue; }
            let aac = mime_full.starts_with("audio/mp4");
            let bitrate = f["bitrate"].as_u64().unwrap_or(0);
            let size = f["contentLength"].as_str().and_then(|v| v.parse().ok()).unwrap_or(0);
            let mime = mime_full.split(';').next().unwrap_or("audio/mp4").trim().to_string();
            let better = match &best { None => true, Some((ba, bb, _)) => (aac, bitrate) > (*ba, *bb) };
            if better {
                best = Some((aac, bitrate, VosStream { url: format!("{u}&cpn={cpn}"), mime, ext: if aac { "m4a" } else { "webm" }, size }));
            }
        }
        best.map(|b| b.2).ok_or_else(|| ApiError::new(502, "YouTube didn't return a usable audio stream"))
    }

    /// Download a video's audio with the visionOS client (see module notes).
    pub async fn vos_download(&self, id: &str, dir: &Path, tx: &watch::Sender<Progress>, on_progress: &(dyn Fn(f64) + Send + Sync)) -> Result<Value, (ApiError, u64)> {
        let st = self.vos_audio(id).await.map_err(|e| (e, 0))?;
        self.fetch_stream(&st.url, &user_agent(), st.size, &format!("audio.{}", st.ext), &st.mime, dir, tx, on_progress).await
    }

    /// Download a googlevideo stream into dir in growing pieces, reporting progress so
    /// playback can start early. Returns the bytes written so far with any error, because
    /// once audio has been handed to the player the caller mustn't switch streams.
    #[allow(clippy::too_many_arguments)]
    pub async fn fetch_stream(&self, url: &str, ua: &str, total: u64, name: &str, mime: &str, dir: &Path,
                              tx: &watch::Sender<Progress>, on_progress: &(dyn Fn(f64) + Send + Sync)) -> Result<Value, (ApiError, u64)> {
        tx.send_modify(|p| { p.kind = "file".into(); p.file = name.to_string(); p.mime = mime.to_string(); p.expected = if total > 0 { Some(total) } else { None }; p.written = 0; p.ready = false; });
        let mut f = tokio::fs::File::create(dir.join(name)).await.map_err(|e| (ApiError::from(e), 0))?;
        let mut written: u64 = 0;
        // googlevideo takes the byte range as a URL parameter (a Range header gets
        // throttled). Start small so playback begins quickly, then fetch bigger pieces.
        let by_param = total > 0 && url.contains("googlevideo.com/videoplayback");
        let mut chunk: u64 = 512 * 1024;
        loop {
            let end = if total > 0 { (written + chunk - 1).min(total - 1) } else { written + chunk - 1 };
            let req = if by_param {
                self.http.get(format!("{url}&range={written}-{end}"))
            } else {
                self.http.get(url).header("Range", format!("bytes={written}-{end}"))
            };
            let res = req.header("User-Agent", ua)
                .header("Origin", "https://www.youtube.com")
                .header("Referer", "https://www.youtube.com/")
                .timeout(std::time::Duration::from_secs(60))
                .send().await.map_err(|e| (ApiError::from(e), written))?;
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
