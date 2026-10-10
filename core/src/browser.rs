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
    /// Arguments that must come right after the program (e.g. `flatpak run … org.mozilla.firefox`).
    pub prefix: Vec<String>,
    /// Where the MixDeck-only browser profile should live, when the browser is sandboxed
    /// (Snap/Flatpak can't read hidden folders in the home directory).
    pub profile_base: Option<PathBuf>,
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
    Some(BrowserInfo { name, exe: Some(PathBuf::from(exe)), kind, prefix: vec![], profile_base: None })
}

/// Linux: ask the desktop (xdg-settings) for the default browser and read its .desktop file.
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
pub fn default_browser() -> Option<BrowserInfo> {
    let out = Command::new("xdg-settings").args(["get", "default-web-browser"]).output().ok()?;
    let desktop_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if desktop_id.is_empty() { return None; }
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.push(std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| home.join(".local/share")));
    let sys = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    dirs.extend(sys.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.push(PathBuf::from("/var/lib/snapd/desktop"));
    dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
    dirs.push(home.join(".local/share/flatpak/exports/share"));
    let text = dirs.iter().map(|d| d.join("applications").join(&desktop_id)).find_map(|p| std::fs::read_to_string(p).ok())?;
    let exec = desktop_exec(&text)?;
    let mut argv: Vec<String> = split_exec(&exec).into_iter()
        .filter(|a| !(a.starts_with('%') && a.len() == 2) && a != "@@u" && a != "@@" && a != "@@U")
        .collect();
    // Skip `env VAR=value …` wrappers.
    if argv.first().map(|a| a == "env").unwrap_or(false) {
        argv.remove(0);
        while argv.first().map(|a| a.contains('=') && !a.starts_with('-')).unwrap_or(false) { argv.remove(0); }
    }
    if argv.is_empty() { return None; }
    let program = argv.remove(0);
    let exe = if program.contains('/') { PathBuf::from(&program) } else { which(&program)? };
    let id_lower = desktop_id.to_lowercase();
    let (kind, name) = {
        let (k, n) = classify(&program);
        if k != Kind::Other { (k, n.to_string()) }
        else if ["chrome", "chromium", "brave", "vivaldi", "edge", "opera"].iter().any(|x| id_lower.contains(x)) { (Kind::Chromium, pretty_name(&id_lower)) }
        else if ["firefox", "librewolf", "waterfox", "floorp", "zen"].iter().any(|x| id_lower.contains(x)) { (Kind::Firefox, pretty_name(&id_lower)) }
        else { (Kind::Other, pretty_name(&id_lower)) }
    };
    // Sandboxed browsers can only use folders inside their own sandbox area.
    let profile_base = if exe.starts_with("/snap/") {
        let snap = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        Some(home.join("snap").join(snap).join("common").join("mixdeck-player"))
    } else if program == "flatpak" {
        argv.iter().find(|a| a.contains('.') && !a.starts_with('-')).map(|app_id| home.join(".var/app").join(app_id).join("mixdeck-player"))
    } else { None };
    Some(BrowserInfo { name, exe: Some(exe), kind, prefix: argv, profile_base })
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
fn pretty_name(id: &str) -> String {
    for (k, n) in [("chrome", "Google Chrome"), ("chromium", "Chromium"), ("brave", "Brave"), ("vivaldi", "Vivaldi"), ("edge", "Microsoft Edge"),
                   ("opera", "Opera"), ("firefox", "Firefox"), ("librewolf", "LibreWolf"), ("waterfox", "Waterfox"), ("floorp", "Floorp"), ("zen", "Zen Browser")] {
        if id.contains(k) { return n.to_string(); }
    }
    id.trim_end_matches(".desktop").to_string()
}

/// The Exec= line of a .desktop file's main section.
pub fn desktop_exec(text: &str) -> Option<String> {
    let mut in_main = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') { in_main = l == "[Desktop Entry]"; continue; }
        if in_main { if let Some(v) = l.strip_prefix("Exec=") { return Some(v.to_string()); } }
    }
    None
}

/// Split an Exec value into arguments, honouring double quotes.
pub fn split_exec(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => { if let Some(n) = chars.next() { cur.push(n); } }
            ' ' | '\t' if !quoted => { if !cur.is_empty() { out.push(std::mem::take(&mut cur)); } }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

#[cfg_attr(not(all(unix, not(target_os = "macos"), not(target_os = "android"))), allow(dead_code))]
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

#[cfg(any(target_os = "macos", target_os = "android"))]
pub fn default_browser() -> Option<BrowserInfo> { None }

#[cfg(windows)]
fn edge_fallback() -> Option<BrowserInfo> {
    let candidates = [
        std::env::var("ProgramFiles(x86)").ok().map(|p| PathBuf::from(p).join(r"Microsoft\Edge\Application\msedge.exe")),
        std::env::var("ProgramFiles").ok().map(|p| PathBuf::from(p).join(r"Microsoft\Edge\Application\msedge.exe")),
    ];
    candidates.into_iter().flatten().find(|p| p.exists()).map(|p| BrowserInfo { name: "Microsoft Edge".into(), exe: Some(p), kind: Kind::Chromium, prefix: vec![], profile_base: None })
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
        let Some(b) = info.filter(|b| b.exe.as_ref().map(|e| e.exists()).unwrap_or(false)) else {
            open::that_detached(&url).map_err(|e| e.to_string())?;
            crate::logf!("[player] opened the player in the default browser (unknown browser)");
            return Ok("your default browser".into());
        };
        let exe = b.exe.clone().unwrap();
        let profiles = b.profile_base.clone().unwrap_or_else(|| self.data_dir().join("browser-player"));
        let _ = std::fs::create_dir_all(&profiles);
        let mut cmd = Command::new(&exe);
        cmd.args(&b.prefix);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_desktop_exec_lines() {
        let chrome = "[Desktop Entry]\nName=Google Chrome\nExec=/usr/bin/google-chrome-stable %U\n[Desktop Action new-window]\nExec=/usr/bin/google-chrome-stable --new-window\n";
        assert_eq!(desktop_exec(chrome).unwrap(), "/usr/bin/google-chrome-stable %U");
        let snap = "[Desktop Entry]\nExec=env BAMF_DESKTOP_FILE_HINT=/var/lib/snapd/desktop/applications/firefox_firefox.desktop /snap/bin/firefox %u\n";
        assert_eq!(split_exec(&desktop_exec(snap).unwrap()), vec!["env", "BAMF_DESKTOP_FILE_HINT=/var/lib/snapd/desktop/applications/firefox_firefox.desktop", "/snap/bin/firefox", "%u"]);
        let flatpak = "[Desktop Entry]\nExec=/usr/bin/flatpak run --branch=stable --arch=x86_64 --command=firefox --file-forwarding org.mozilla.firefox @@u %u @@\n";
        let argv = split_exec(&desktop_exec(flatpak).unwrap());
        assert_eq!(argv[0], "/usr/bin/flatpak");
        assert!(argv.contains(&"org.mozilla.firefox".to_string()));
        assert_eq!(split_exec(r#""/opt/My Browser/browser" --flag "%U""#), vec!["/opt/My Browser/browser", "--flag", "%U"]);
    }

    #[test]
    fn classifies_browsers() {
        assert_eq!(classify("/usr/bin/brave-browser").0, Kind::Chromium);
        assert_eq!(classify(r"C:\Program Files\Mozilla Firefox\firefox.exe").0, Kind::Firefox);
        assert_eq!(classify("/usr/bin/epiphany").0, Kind::Other);
    }

    #[test]
    fn windows_command_values() {
        assert_eq!(exe_from_command(r#""C:\Program Files\Google\Chrome\Application\chrome.exe" --single-argument %1"#).unwrap(), r"C:\Program Files\Google\Chrome\Application\chrome.exe");
        assert_eq!(exe_from_command(r"C:\Opera\launcher.exe -noautoupdate -- %1").unwrap(), r"C:\Opera\launcher.exe");
    }
}
