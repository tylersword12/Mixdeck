// Running the audio engine in the user's default browser.
//
// The player normally runs inside MixDeck's own window, but Spotify's player needs browser
// DRM (Widevine) that the built-in Windows web view doesn't reliably provide. When that
// happens, the player moves to the default browser, launched as a separate instance with
// its own MixDeck profile so the user's normal tabs and logins aren't touched.
use crate::app::App;
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub enum Kind { Chromium, Firefox, Other }

#[derive(Clone, Debug)]
pub struct BrowserInfo {
    pub name: String,
    pub exe: Option<PathBuf>,
    pub kind: Kind,
}

#[cfg_attr(not(windows), allow(dead_code))]
fn classify(exe: &str) -> (Kind, &'static str) {
    let f = exe.rsplit(['\\', '/']).next().unwrap_or(exe).to_lowercase();
    match f.as_str() {
        "chrome.exe" | "chrome" | "google-chrome" | "google-chrome-stable" => (Kind::Chromium, "Google Chrome"),
        "msedge.exe" | "msedge" | "microsoft-edge" => (Kind::Chromium, "Microsoft Edge"),
        "brave.exe" | "brave" | "brave-browser" => (Kind::Chromium, "Brave"),
        "vivaldi.exe" | "vivaldi" => (Kind::Chromium, "Vivaldi"),
        "opera.exe" | "launcher.exe" | "opera" => (Kind::Chromium, "Opera"),
        "chromium.exe" | "chromium" | "chromium-browser" => (Kind::Chromium, "Chromium"),
        "browser.exe" => (Kind::Chromium, "Yandex Browser"),
        "arc.exe" => (Kind::Chromium, "Arc"),
        "firefox.exe" | "firefox" => (Kind::Firefox, "Firefox"),
        "librewolf.exe" | "librewolf" => (Kind::Firefox, "LibreWolf"),
        "waterfox.exe" | "waterfox" => (Kind::Firefox, "Waterfox"),
        "floorp.exe" | "floorp" => (Kind::Firefox, "Floorp"),
        "zen.exe" | "zen" => (Kind::Firefox, "Zen Browser"),
        _ => (Kind::Other, "your default browser"),
    }
}

/// Parse the exe path out of a registry "shell\open\command" value.
pub fn exe_from_command(cmd: &str) -> Option<String> {
    let c = cmd.trim();
    if let Some(rest) = c.strip_prefix('"') {
        return rest.split('"').next().map(|s| s.to_string());
    }
    let lower = c.to_lowercase();
    lower.find(".exe").map(|i| c[..i + 4].to_string())
}

#[cfg(windows)]
pub fn default_browser() -> Option<BrowserInfo> {
    use winreg::enums::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    // Newer Windows 11 builds use UserChoiceLatest; older ones UserChoice.
    let mut progid: Option<String> = None;
    for key in [
        r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoiceLatest",
        r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\https\UserChoice",
        r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice",
    ] {
        if let Ok(k) = hkcu.open_subkey(key) {
            if let Ok(p) = k.get_value::<String, _>("ProgId") { if !p.is_empty() { progid = Some(p); break; } }
        }
    }
    let progid = progid?;
    let hkcr = RegKey::predef(HKEY_CLASSES_ROOT);
    let cmd: String = hkcr.open_subkey(format!(r"{progid}\shell\open\command")).ok()?.get_value("").ok()?;
    let exe = exe_from_command(&cmd)?;
    let (kind, name) = classify(&exe);
    let name = if kind == Kind::Other {
        PathBuf::from(&exe).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| name.to_string())
    } else { name.to_string() };
    Some(BrowserInfo { name, exe: Some(PathBuf::from(exe)), kind })
}

#[cfg(not(windows))]
pub fn default_browser() -> Option<BrowserInfo> {
    // Desktop Linux/macOS: not the target platform; open the page normally.
    None
}

#[cfg(windows)]
fn edge_fallback() -> Option<BrowserInfo> {
    let candidates = [
        std::env::var("ProgramFiles(x86)").ok().map(|p| PathBuf::from(p).join(r"Microsoft\Edge\Application\msedge.exe")),
        std::env::var("ProgramFiles").ok().map(|p| PathBuf::from(p).join(r"Microsoft\Edge\Application\msedge.exe")),
    ];
    candidates.into_iter().flatten().find(|p| p.exists()).map(|p| BrowserInfo { name: "Microsoft Edge".into(), exe: Some(p), kind: Kind::Chromium })
}
#[cfg(not(windows))]
fn edge_fallback() -> Option<BrowserInfo> { None }

impl App {
    pub fn player_url(&self, host: &str) -> String { format!("http://127.0.0.1:{}/player?auto=1&host={host}", self.port()) }

    /// Start the player in the default browser. Returns the browser's name.
    pub fn launch_browser_player(&self) -> Result<String, String> {
        self.kill_browser_player();
        let url = self.player_url("browser");
        let info = default_browser().or_else(edge_fallback);
        let profiles = self.data_dir().join("browser-player");
        let _ = std::fs::create_dir_all(&profiles);
        let Some(b) = info.filter(|b| b.exe.as_ref().map(|e| e.exists()).unwrap_or(false)) else {
            open::that_detached(&url).map_err(|e| e.to_string())?;
            crate::logf!("[player] opened the player in the default browser (unknown browser)");
            return Ok("your default browser".into());
        };
        let exe = b.exe.clone().unwrap();
        let mut cmd = Command::new(&exe);
        match b.kind {
            Kind::Chromium => {
                let profile = profiles.join("chromium");
                cmd.arg(format!("--app={url}"))
                    .arg(format!("--user-data-dir={}", profile.display()))
                    .args([
                        "--no-first-run", "--no-default-browser-check",
                        "--autoplay-policy=no-user-gesture-required",
                        "--disable-background-timer-throttling", "--disable-renderer-backgrounding",
                        "--disable-backgrounding-occluded-windows",
                        "--window-size=520,760",
                    ]);
            }
            Kind::Firefox => {
                let profile = profiles.join("firefox");
                let _ = std::fs::create_dir_all(&profile);
                // Allow autoplay and DRM in the MixDeck-only profile; skip first-run pages.
                let prefs = [
                    ("media.autoplay.default", "0"),
                    ("media.autoplay.blocking_policy", "0"),
                    ("media.eme.enabled", "true"),
                    ("media.gmp-widevinecdm.enabled", "true"),
                    ("media.gmp-widevinecdm.visible", "true"),
                    ("browser.shell.checkDefaultBrowser", "false"),
                    ("browser.aboutwelcome.enabled", "false"),
                    ("browser.startup.homepage_override.mstone", "\"ignore\""),
                    ("datareporting.policy.dataSubmissionPolicyBypassNotification", "true"),
                    ("toolkit.telemetry.reportingpolicy.firstRun", "false"),
                    ("dom.min_background_timeout_value", "4"),
                ];
                let body: String = prefs.iter().map(|(k, v)| format!("user_pref(\"{k}\", {v});\n")).collect();
                let _ = std::fs::write(profile.join("user.js"), body);
                cmd.arg("-profile").arg(&profile).arg("-no-remote").arg("-new-window").arg(&url);
            }
            Kind::Other => {
                open::that_detached(&url).map_err(|e| e.to_string())?;
                crate::logf!("[player] opened the player in {}", b.name);
                return Ok(b.name);
            }
        }
        let child = cmd.spawn().map_err(|e| format!("Couldn't start {}: {e}", b.name))?;
        *self.browser_child.lock() = Some(child);
        crate::logf!("[player] started the player in {} ({})", b.name, exe.display());
        Ok(b.name)
    }
}
