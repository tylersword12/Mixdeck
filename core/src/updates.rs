// Updates from the GitHub "latest" release. Every CI build is numbered (MIXDECK_BUILD, the
// Actions run number) and the release carries version.json with that number and the list
// of files it contains. On start MixDeck compares its own number with the release's; the
// app then offers the update when you close it (Android: when it opens).
use crate::app::App;
use crate::util::{now_ms, ApiError, ApiResult};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub const REPO: &str = "tylersword12/Mixdeck";

/// This build's number; 0 for builds made outside GitHub Actions (never offered updates).
pub fn current_build() -> u64 { option_env!("MIXDECK_BUILD").and_then(|b| b.parse().ok()).unwrap_or(0) }

pub fn version_string() -> String {
    option_env!("MIXDECK_VERSION").map(str::to_string).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateState {
    pub current: u64,
    pub latest: Option<u64>,
    pub latest_version: Option<String>,
    pub available: bool,
    pub asset: Option<String>,
    pub checked_at: i64,
    pub checking: bool,
    pub error: Option<String>,
    /// Download progress 0..1 while installing.
    pub installing: Option<f64>,
}

static STATE: Lazy<Mutex<UpdateState>> = Lazy::new(|| Mutex::new(UpdateState { current: current_build(), ..Default::default() }));

pub fn state() -> UpdateState { STATE.lock().clone() }

fn base_url() -> String {
    std::env::var("MIXDECK_UPDATE_URL").unwrap_or_else(|_| format!("https://github.com/{REPO}/releases/download/latest"))
}

impl App {
    /// The release file that updates this copy of MixDeck.
    pub fn update_asset(&self) -> &'static str {
        match self.host.platform() {
            "windows" => "MixDeck-Setup.exe",
            "android" => "MixDeck-Android.apk",
            _ => {
                if std::env::var_os("APPIMAGE").is_some() { "MixDeck-linux-x86_64.AppImage" }
                else if Path::new("/usr/bin/dpkg").exists() { "MixDeck-linux-amd64.deb" }
                else { "MixDeck-linux-x86_64.rpm" }
            }
        }
    }

    pub async fn check_updates(&self) -> UpdateState {
        STATE.lock().checking = true;
        let r = self.fetch_version().await;
        {
            let mut st = STATE.lock();
            st.checking = false;
            st.checked_at = now_ms();
            match r {
                Ok(v) => {
                    let build = v["build"].as_u64().unwrap_or(0);
                    let asset = self.update_asset();
                    let has_asset = v["assets"].as_array().map(|a| a.iter().any(|x| x.as_str() == Some(asset))).unwrap_or(false);
                    st.latest = Some(build);
                    st.latest_version = v["version"].as_str().map(str::to_string);
                    st.asset = Some(asset.to_string());
                    st.error = None;
                    st.available = st.current > 0 && build > st.current && has_asset;
                    if st.available { crate::logf!("[update] build {build} is available (this is build {})", st.current); }
                }
                Err(e) => { st.error = Some(e.message.clone()); crate::logf!("[update] check failed: {}", e.message); }
            }
        }
        self.broadcast_info();
        state()
    }

    async fn fetch_version(&self) -> ApiResult<Value> {
        let res = self.http.get(format!("{}/version.json", base_url())).send().await
            .map_err(|_| ApiError::new(502, "Couldn't reach GitHub to check for updates"))?;
        if res.status().as_u16() == 404 { return Err(ApiError::new(502, "No published release found on GitHub yet")); }
        if !res.status().is_success() { return Err(ApiError::new(502, format!("GitHub returned HTTP {}", res.status().as_u16()))); }
        res.json().await.map_err(|_| ApiError::new(502, "The release's version.json is unreadable"))
    }

    /// Download the update and start installing it. Returns once the installer has been
    /// launched; the caller then quits MixDeck (the installer replaces the files).
    /// Android: opens the APK download in the browser, where you tap it to install.
    pub async fn install_update(&self, relaunch: bool) -> ApiResult<Value> {
        let asset = self.update_asset();
        let url = format!("{}/{asset}", base_url());
        if self.host.platform() == "android" {
            self.host.open_url(&url);
            return Ok(json!({ "opened": true }));
        }
        if STATE.lock().installing.is_some() { return Err(ApiError::bad("The update is already downloading")); }
        STATE.lock().installing = Some(0.0);
        let r = self.download_and_launch(&url, asset, relaunch).await;
        if r.is_err() { STATE.lock().installing = None; }
        self.broadcast_info();
        r
    }

    async fn download_and_launch(&self, url: &str, asset: &str, relaunch: bool) -> ApiResult<Value> {
        let dir = self.data_dir().join("updates");
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(asset);
        crate::logf!("[update] downloading {asset}");
        let mut res = self.http.get(url).timeout(std::time::Duration::from_secs(1800)).send().await?;
        if !res.status().is_success() { return Err(ApiError::new(502, format!("Download failed: HTTP {}", res.status().as_u16()))); }
        let total = res.content_length().unwrap_or(0);
        let tmp = path.with_extension("part");
        let mut f = tokio::fs::File::create(&tmp).await?;
        let mut got: u64 = 0;
        let mut last_report = 0i64;
        while let Some(chunk) = res.chunk().await? {
            f.write_all(&chunk).await?;
            got += chunk.len() as u64;
            if total > 0 && now_ms() - last_report > 500 {
                last_report = now_ms();
                STATE.lock().installing = Some(got as f64 / total as f64);
                self.broadcast_info();
            }
        }
        f.flush().await?;
        drop(f);
        if total > 0 && got < total { return Err(ApiError::new(502, "The download was cut short; try again")); }
        tokio::fs::rename(&tmp, &path).await?;
        crate::logf!("[update] downloaded {} MB, installing", got / 1_048_576);
        launch_installer(&path, relaunch).map_err(|e| ApiError::new(500, format!("Couldn't start the installer: {e}")))?;
        Ok(json!({ "installing": true }))
    }
}

/// Start the installer in a separate process that outlives MixDeck.
fn launch_installer(path: &Path, relaunch: bool) -> std::io::Result<()> {
    let me = std::env::current_exe().unwrap_or_default();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // Wait for MixDeck to exit, install silently (per-user, no admin prompt), then
        // optionally start the new version.
        let mut script = format!("ping -n 3 127.0.0.1 >nul & start \"\" /wait \"{}\" /S", path.display());
        if relaunch { script.push_str(&format!(" & start \"\" \"{}\"", me.display())); }
        std::process::Command::new("cmd").raw_arg(format!("/C \"{script}\""))
            .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW).spawn()?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let p = path.display().to_string();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        if name.ends_with(".AppImage") {
            // Replace the AppImage file in place; the running copy keeps its old inode.
            let target = PathBuf::from(std::env::var_os("APPIMAGE").unwrap_or_default());
            let staged = target.with_extension("AppImage.new");
            std::fs::copy(path, &staged)?;
            #[cfg(unix)]
            { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?; }
            std::fs::rename(&staged, &target)?;
            let _ = std::fs::remove_file(path);
            if relaunch { std::process::Command::new("sh").arg("-c").arg("sleep 2; exec \"$0\"").arg(&target).spawn()?; }
            return Ok(());
        }
        // .deb / .rpm: install with the desktop's password prompt (pkexec), falling back to
        // opening the package in the software installer.
        let install = if name.ends_with(".deb") {
            r#"pkexec apt-get install -y "$0" || pkexec dpkg -i "$0" || xdg-open "$0""#
        } else {
            r#"pkexec dnf install -y "$0" || pkexec zypper --non-interactive install --allow-unsigned-rpm "$0" || pkexec rpm -U "$0" || xdg-open "$0""#
        };
        let script = if relaunch { format!("sleep 1; {install} && (\"$1\" >/dev/null 2>&1 &)") } else { format!("sleep 1; {install}") };
        std::process::Command::new("setsid").arg("sh").arg("-c").arg(script).arg(&p).arg(&me)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn()
            .or_else(|_| std::process::Command::new("sh").arg("-c").arg(format!("sleep 1; {install}")).arg(&p).spawn())?;
        Ok(())
    }
}
