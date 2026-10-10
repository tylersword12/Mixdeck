// YouTube through yt-dlp. Same idea as the SoundCloud code (use what the website itself
// uses, no API key), but YouTube changes so often that yt-dlp — maintained daily — does
// the extraction. MixDeck downloads yt-dlp itself, plus a JavaScript runtime that yt-dlp
// needs for YouTube (Node.js 22+ if installed, otherwise Deno).
#![cfg_attr(feature = "native-youtube", allow(unreachable_code, unused_imports, unused_variables))]
use crate::app::App;
use crate::util::{num, s, ApiError, ApiResult};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const YTDLP_RELEASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/";
const DENO_RELEASE: &str = "https://github.com/denoland/deno/releases/latest/download/";

fn ytdlp_asset() -> &'static str {
    if cfg!(windows) { "yt-dlp.exe" } else if cfg!(target_os = "macos") { "yt-dlp_macos" } else { "yt-dlp_linux" }
}
fn deno_asset() -> &'static str {
    if cfg!(windows) { "deno-x86_64-pc-windows-msvc.zip" }
    else if cfg!(all(target_os = "macos", target_arch = "aarch64")) { "deno-aarch64-apple-darwin.zip" }
    else if cfg!(target_os = "macos") { "deno-x86_64-apple-darwin.zip" }
    else { "deno-x86_64-unknown-linux-gnu.zip" }
}
fn exe(name: &str) -> String { if cfg!(windows) { format!("{name}.exe") } else { name.to_string() } }

pub fn valid_id(id: &str) -> bool { id.len() == 11 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') }

/// A command that never flashes a console window on Windows.
pub fn quiet_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c.kill_on_drop(true);
    c
}

#[derive(Default)]
pub struct ToolsState {
    /// { running, step, progress, error } while installing yt-dlp / Deno
    pub install: Mutex<Value>,
    pub install_lock: tokio::sync::Mutex<()>,
    js_cache: Mutex<Option<(String, PathBuf)>>,
}

impl App {
    fn bin_dir(&self) -> PathBuf { self.data_dir().join("bin") }
    fn local_ytdlp(&self) -> PathBuf { self.bin_dir().join(exe("yt-dlp")) }
    fn local_deno(&self) -> PathBuf { self.bin_dir().join(exe("deno")) }

    pub fn ytdlp_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.opts.ytdlp_path { return Some(p.clone()); }
        let p = self.local_ytdlp();
        p.exists().then_some(p)
    }

    /// The JS runtime yt-dlp should use: ("node" | "deno", path).
    pub async fn js_runtime(&self) -> Option<(String, PathBuf)> {
        if let Some(c) = self.tools.js_cache.lock().clone() { return Some(c); }
        let found = self.find_js_runtime().await;
        *self.tools.js_cache.lock() = found.clone();
        found
    }

    async fn find_js_runtime(&self) -> Option<(String, PathBuf)> {
        // Node.js 22+ on PATH (yt-dlp's minimum for Node).
        if let Ok(out) = quiet_command(exe("node")).arg("--version").output().await {
            let v = String::from_utf8_lossy(&out.stdout).trim().trim_start_matches('v').to_string();
            if v.split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0) >= 22 {
                let path = which(&exe("node")).unwrap_or_else(|| PathBuf::from(exe("node")));
                return Some(("node".into(), path));
            }
        }
        let deno = self.local_deno();
        if deno.exists() { return Some(("deno".into(), deno)); }
        if let Some(p) = which(&exe("deno")) { return Some(("deno".into(), p)); }
        None
    }

    async fn base_args(&self) -> Vec<String> {
        let mut a: Vec<String> = ["--ignore-config", "--no-warnings", "--color", "never", "--no-cache-dir"].iter().map(|s| s.to_string()).collect();
        if let Some((kind, path)) = self.js_runtime().await {
            a.push("--js-runtimes".into());
            a.push(format!("{kind}:{}", path.display()));
        }
        a
    }

    /// Run yt-dlp and return stdout. `on_line` sees every stdout/stderr line as it arrives.
    pub async fn ytdlp_run(&self, args: &[String], timeout_secs: u64, on_line: Option<Arc<dyn Fn(&str) + Send + Sync>>) -> ApiResult<String> {
        if self.ytdlp_path().is_none() { self.tools_install(false).await?; }
        let bin = self.ytdlp_path().ok_or_else(|| ApiError::internal("yt-dlp is not installed"))?;
        let mut all = self.base_args().await;
        all.extend(args.iter().cloned());
        let mut child = quiet_command(&bin).args(&all).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()
            .map_err(|e| ApiError::internal(format!("Could not run yt-dlp: {e}")))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let cb1 = on_line.clone();
        let out_task = tokio::spawn(async move {
            let mut out = String::new();
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Some(cb) = &cb1 { cb(&l); }
                out.push_str(&l);
                out.push('\n');
            }
            out
        });
        let cb2 = on_line.clone();
        let err_task = tokio::spawn(async move {
            let mut err = String::new();
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Some(cb) = &cb2 { cb(&l); }
                err.push_str(&l);
                err.push('\n');
            }
            err
        });
        let status = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait()).await {
            Ok(r) => r?,
            Err(_) => { let _ = child.kill().await; return Err(ApiError::new(504, "yt-dlp timed out")); }
        };
        let out = out_task.await.unwrap_or_default();
        let err = err_task.await.unwrap_or_default();
        if status.success() { return Ok(out); }
        let last = err.lines().filter(|l| l.contains("ERROR:")).last().map(|s| s.to_string())
            .or_else(|| err.lines().last().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("yt-dlp exited with {status}"));
        crate::logf!("[youtube] {last}");
        let cleaned = regex::Regex::new(r"^ERROR:\s*(\[[^\]]+\]\s*)?([\w-]{11}:\s*)?").unwrap().replace(&last, "").to_string();
        Err(ApiError::new(502, friendly_error(&cleaned)))
    }

    pub fn yt_map_entry(e: &Value) -> Option<Value> {
        let id = s(&e["id"]);
        if !valid_id(&id) { return None; }
        let live = s(&e["live_status"]) == "is_live" || e["is_live"] == json!(true);
        let avail = s(&e["availability"]);
        let restricted = ["private", "needs_auth", "subscriber_only", "premium_only"].iter().any(|x| avail.contains(x));
        let pick = |keys: &[&str]| keys.iter().map(|k| s(&e[*k])).find(|v| !v.is_empty()).unwrap_or_default();
        let title = pick(&["track", "title"]);
        Some(json!({
            "source": "youtube", "id": id,
            "title": if title.is_empty() { "(untitled)".to_string() } else { title },
            "artist": pick(&["artist", "creator", "channel", "uploader"]),
            "duration": (num(&e["duration"], 0.0) * 1000.0).round(),
            "artwork": format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"),
            "url": format!("https://www.youtube.com/watch?v={id}"),
            "playable": if live || restricted { "unavailable" } else { "ok" },
        }))
    }

    pub async fn yt_search(&self, q: &str, offset: u64) -> ApiResult {
        #[cfg(feature = "native-youtube")]
        return self.ytn_search(q, offset).await;
        let n = offset + 20;
        let out = self.ytdlp_run(&["--flat-playlist".into(), "-J".into(), format!("ytsearch{n}:{q}")], 60, None).await?;
        let j: Value = serde_json::from_str(&out)?;
        let entries = j["entries"].as_array().cloned().unwrap_or_default();
        let items: Vec<Value> = entries.iter().skip(offset as usize).filter_map(Self::yt_map_entry)
            .filter(|t| { let d = num(&t["duration"], 0.0); d == 0.0 || d < 3.0 * 3600e3 }) // skip 10-hour loops
            .collect();
        let next = if entries.len() as u64 >= n && n < 100 { json!(n) } else { Value::Null };
        Ok(json!({ "items": items, "next": next }))
    }

    pub async fn yt_resolve(&self, link: &str) -> ApiResult {
        #[cfg(feature = "native-youtube")]
        return self.ytn_resolve(link).await;
        match parse_url(link) {
            Some(YtLink::Video(id)) => {
                let out = self.ytdlp_run(&["--flat-playlist".into(), "--no-playlist".into(), "-J".into(), format!("https://www.youtube.com/watch?v={id}")], 60, None).await?;
                let j: Value = serde_json::from_str(&out)?;
                Ok(json!({ "kind": "track", "tracks": Self::yt_map_entry(&j).into_iter().collect::<Vec<_>>() }))
            }
            Some(YtLink::Playlist(list)) => {
                let out = self.ytdlp_run(&["--flat-playlist".into(), "-J".into(), format!("https://www.youtube.com/playlist?list={list}")], 120, None).await?;
                let j: Value = serde_json::from_str(&out)?;
                let tracks: Vec<Value> = j["entries"].as_array().cloned().unwrap_or_default().iter().filter_map(Self::yt_map_entry)
                    .filter(|t| !matches!(s(&t["title"]).as_str(), "[Deleted video]" | "[Private video]")).collect();
                if tracks.is_empty() { return Err(ApiError::bad("That playlist is empty or private")); }
                let name = if j["title"].is_string() { s(&j["title"]) } else { "YouTube playlist".into() };
                Ok(json!({ "kind": "playlist", "name": name, "tracks": tracks }))
            }
            None => Err(ApiError::bad("Paste a YouTube video or playlist link")),
        }
    }

    /// Download the audio-only stream into dir, reporting progress through `tx` so the
    /// player can start on the file while it's still arriving. Prefers AAC (m4a).
    pub async fn yt_download(&self, id: &str, dir: &Path, tx: tokio::sync::watch::Sender<crate::cache::Progress>, on_progress: impl Fn(f64) + Send + Sync + 'static) -> ApiResult {
        if !valid_id(id) { return Err(ApiError::bad("bad video id")); }
        #[cfg(feature = "native-youtube")]
        return self.ytn_download(id, dir, tx, on_progress).await;
        #[allow(unreachable_code)]
        let file: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let f2 = file.clone();
        let tx2 = tx.clone();
        let re_prog = regex::Regex::new(r"^PROGRESS\s+([\d.]+)%").unwrap();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
            if let Some(c) = re_prog.captures(line) { on_progress(c[1].parse::<f64>().unwrap_or(0.0) / 100.0); }
            // Printed just before the download starts: the extension and (usually) exact size.
            if let Some(rest) = line.strip_prefix("START ") {
                let mut parts = rest.split_whitespace();
                let ext = parts.next().unwrap_or("m4a").to_string();
                let size = parts.next().and_then(|v| v.parse::<u64>().ok());
                let name = format!("audio.{ext}");
                *f2.lock() = Some(name.clone());
                let mime = mime_for(&ext).to_string();
                tx2.send_modify(|p| { p.kind = "file".into(); p.file = name; p.mime = mime; p.expected = size; });
            }
            if let Some(rest) = line.strip_prefix("FILE ") {
                if let Some(n) = Path::new(rest.trim()).file_name() { *f2.lock() = Some(n.to_string_lossy().to_string()); }
            }
        });
        // Watch the file grow so readers know how much is safe to send.
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let poller = {
            let stop = stop.clone();
            let file = file.clone();
            let dir = dir.to_path_buf();
            let tx = tx.clone();
            tokio::spawn(async move {
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    let name = file.lock().clone();
                    if let Some(name) = name {
                        if let Ok(md) = tokio::fs::metadata(dir.join(&name)).await {
                            let n = md.len();
                            tx.send_modify(|p| {
                                p.written = n;
                                if n >= crate::cache_start_bytes().min(p.expected.unwrap_or(u64::MAX)) { p.ready = true; }
                            });
                        }
                    }
                }
            })
        };
        let template = dir.join("audio.%(ext)s").display().to_string();
        let args: Vec<String> = [
            "-f", "bestaudio[ext=m4a]/bestaudio[acodec^=opus]/bestaudio/best",
            "--no-playlist", "--no-part", "--no-mtime", "--progress", "--newline",
            "--progress-template", "download:PROGRESS %(progress._percent_str)s",
            "-o", &template,
            "--print", "before_dl:START %(ext)s %(filesize)s",
            "--print", "after_move:FILE %(filepath)s",
        ].iter().map(|s| s.to_string()).chain(std::iter::once(format!("https://www.youtube.com/watch?v={id}"))).collect();
        let result = self.ytdlp_run(&args, 600, Some(cb)).await;
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = poller.await;
        result?;
        let mut name = file.lock().clone();
        if name.as_ref().map(|n| !dir.join(n).exists()).unwrap_or(true) {
            name = None;
            if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
                while let Ok(Some(e)) = rd.next_entry().await {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n.starts_with("audio.") { name = Some(n); break; }
                }
            }
        }
        let name = name.ok_or_else(|| ApiError::internal("yt-dlp finished but no audio file was written"))?;
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        let bytes = tokio::fs::metadata(dir.join(&name)).await.map(|m| m.len()).unwrap_or(0);
        let mime = mime_for(&ext);
        tx.send_modify(|p| { p.kind = "file".into(); p.file = name.clone(); p.mime = mime.into(); p.written = bytes; p.expected = Some(bytes); });
        Ok(json!({ "type": "file", "file": name, "mime": mime, "bytes": bytes }))
    }

    // ---- tools: install / update / status --------------------------------------

    pub async fn tools_status(&self) -> Value {
        #[cfg(feature = "native-youtube")]
        return json!({ "ytdlp": { "installed": true, "version": "built-in" }, "js": { "kind": "built-in", "path": "" }, "ready": true, "builtin": true, "install": self.tools.install.lock().clone() });
        #[allow(unreachable_code)]
        let yt = self.ytdlp_path();
        let mut version = Value::Null;
        if let Some(p) = &yt {
            if let Ok(o) = quiet_command(p).arg("--version").output().await {
                version = json!(String::from_utf8_lossy(&o.stdout).trim().to_string());
            }
        }
        let js = self.js_runtime().await;
        json!({
            "ytdlp": { "installed": yt.is_some(), "version": version },
            "js": js.map(|(k, p)| json!({ "kind": k, "path": p.display().to_string() })),
            "ready": yt.is_some() && self.tools.js_cache.lock().is_some(),
            "install": self.tools.install.lock().clone(),
        })
    }

    fn install_progress(&self, step: &str, progress: f64) {
        *self.tools.install.lock() = json!({ "running": true, "step": step, "progress": progress });
    }

    async fn download_to(&self, url: &str, dest: &Path, step: &str) -> ApiResult<()> {
        let res = self.http.get(url).timeout(std::time::Duration::from_secs(900)).send().await?;
        if !res.status().is_success() {
            return Err(ApiError::new(502, format!("Download failed (HTTP {}) for {url}", res.status().as_u16())));
        }
        let total = res.content_length().unwrap_or(0) as f64;
        let tmp = dest.with_extension("download");
        let mut f = tokio::fs::File::create(&tmp).await?;
        let mut got = 0f64;
        let mut stream = res.bytes_stream();
        use futures::StreamExt;
        use tokio::io::AsyncWriteExt;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            got += chunk.len() as f64;
            f.write_all(&chunk).await?;
            if total > 0.0 { self.install_progress(step, got / total); }
        }
        f.flush().await?;
        drop(f);
        tokio::fs::rename(&tmp, dest).await?;
        Ok(())
    }

    /// Install whatever is missing (yt-dlp and, without Node 22+, Deno). Safe to call often.
    pub async fn tools_install(&self, force_update: bool) -> ApiResult<()> {
        #[cfg(feature = "native-youtube")]
        { let _ = force_update; return Ok(()); }
        #[allow(unreachable_code)]
        let _g = self.tools.install_lock.lock().await;
        let r = self.tools_install_inner(force_update).await;
        match &r {
            Ok(()) => *self.tools.install.lock() = json!({ "running": false, "done": true }),
            Err(e) => *self.tools.install.lock() = json!({ "running": false, "error": e.message }),
        }
        r
    }

    async fn tools_install_inner(&self, force_update: bool) -> ApiResult<()> {
        tokio::fs::create_dir_all(self.bin_dir()).await?;
        if self.opts.ytdlp_path.is_none() && (force_update || !self.local_ytdlp().exists()) {
            self.install_progress("Downloading yt-dlp", 0.0);
            crate::logf!("[youtube] downloading yt-dlp");
            let dest = self.local_ytdlp();
            self.download_to(&format!("{YTDLP_RELEASE}{}", ytdlp_asset()), &dest, "Downloading yt-dlp").await?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
            }
        }
        *self.tools.js_cache.lock() = None;
        if self.js_runtime().await.is_none() {
            self.install_progress("Downloading Deno (JavaScript runtime for YouTube)", 0.0);
            crate::logf!("[youtube] downloading deno");
            let zip_path = self.bin_dir().join("deno.zip");
            self.download_to(&format!("{DENO_RELEASE}{}", deno_asset()), &zip_path, "Downloading Deno (JavaScript runtime for YouTube)").await?;
            self.install_progress("Unpacking Deno", 1.0);
            let bin_dir = self.bin_dir();
            let target = self.local_deno();
            tokio::task::spawn_blocking(move || -> Result<(), String> {
                let f = std::fs::File::open(&zip_path).map_err(|e| e.to_string())?;
                let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
                let name = exe("deno");
                let mut entry = z.by_name(&name).map_err(|e| format!("deno not found in archive: {e}"))?;
                let mut out = std::fs::File::create(&target).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
                }
                let _ = std::fs::remove_file(&zip_path);
                let _ = bin_dir;
                Ok(())
            }).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(ApiError::internal)?;
            *self.tools.js_cache.lock() = None;
        }
        Ok(())
    }

    pub async fn tools_update(&self) -> ApiResult {
        #[cfg(feature = "native-youtube")]
        return Ok(json!({ "output": "YouTube support is built into this version; update MixDeck to update it." }));
        #[allow(unreachable_code)]
        if self.opts.ytdlp_path.is_some() { return Ok(json!({ "output": "Using a custom yt-dlp (YTDLP_PATH); update it yourself." })); }
        if self.ytdlp_path().is_none() { self.tools_install(false).await?; return Ok(json!({ "output": "Installed" })); }
        // yt-dlp's own updater; fall back to re-downloading the release.
        match self.ytdlp_run(&["-U".into()], 300, None).await {
            Ok(out) => Ok(json!({ "output": out.trim().lines().last().unwrap_or("Up to date") })),
            Err(_) => { self.tools_install(true).await?; Ok(json!({ "output": "Re-downloaded the latest yt-dlp" })) }
        }
    }
}

pub fn mime_for(ext: &str) -> &'static str {
    match ext { "m4a" | "mp4" => "audio/mp4", "webm" => "audio/webm", "opus" | "ogg" => "audio/ogg", "mp3" => "audio/mpeg", _ => "application/octet-stream" }
}

pub fn friendly_error(m: &str) -> String {
    let l = m.to_lowercase();
    if l.contains("sign in to confirm your age") { return "Age-restricted video (needs a signed-in YouTube account)".into(); }
    if l.contains("not a bot") { return "YouTube is asking for a bot check. Try again later, or update yt-dlp in Settings".into(); }
    if l.contains("private video") { return "Private video".into(); }
    if l.contains("video unavailable") || l.contains("this video is not available") { return "Video unavailable (removed or blocked in your country)".into(); }
    if l.contains("members-only") || l.contains("join this channel") { return "Members-only video".into(); }
    m.to_string()
}

pub enum YtLink { Video(String), Playlist(String) }

pub fn parse_url(link: &str) -> Option<YtLink> {
    let full = if link.to_lowercase().starts_with("http") { link.to_string() } else { format!("https://{link}") };
    let u = url::Url::parse(&full).ok()?;
    let host = u.host_str()?.to_lowercase();
    let host = host.trim_start_matches("www.").trim_start_matches("m.").trim_start_matches("music.");
    if host == "youtu.be" {
        let id: String = u.path().trim_start_matches('/').chars().take(11).collect();
        return valid_id(&id).then_some(YtLink::Video(id));
    }
    if host != "youtube.com" && host != "youtube-nocookie.com" { return None; }
    let q: std::collections::HashMap<String, String> = u.query_pairs().into_owned().collect();
    let path = u.path();
    if path == "/playlist" { if let Some(l) = q.get("list") { return Some(YtLink::Playlist(l.clone())); } }
    if let Some(v) = q.get("v") { if valid_id(v) { return Some(YtLink::Video(v.clone())); } }
    for prefix in ["/shorts/", "/embed/", "/live/", "/v/"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            let id: String = rest.chars().take(11).collect();
            if valid_id(&id) { return Some(YtLink::Video(id)); }
        }
    }
    q.get("list").map(|l| YtLink::Playlist(l.clone()))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}
