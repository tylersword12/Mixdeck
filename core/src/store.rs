// JSON-file persistence plus the data models saved in the data folder.
// Field names match the earlier Node.js version so its data folder can be imported as-is.
use crate::util::{de_f64_lenient, de_opt_string_lenient, de_string_lenient};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Store<T> {
    path: PathBuf,
    pub data: T,
}

impl<T: Serialize + DeserializeOwned + Default> Store<T> {
    pub fn load(dir: &Path, name: &str) -> Self {
        let path = dir.join(format!("{name}.json"));
        let data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| match serde_json::from_str::<T>(&s) {
                Ok(v) => Some(v),
                Err(e) => {
                    crate::logf!("[store] {name}.json is unreadable ({e}); starting fresh and keeping a backup");
                    let _ = std::fs::copy(&path, dir.join(format!("{name}.broken.json")));
                    None
                }
            })
            .unwrap_or_default();
        Store { path, data }
    }

    pub fn save(&self) {
        let tmp = self.path.with_extension("json.tmp");
        match serde_json::to_vec_pretty(&self.data) {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &self.path)) {
                    crate::logf!("[store] could not save {}: {e}", self.path.display());
                }
            }
            Err(e) => crate::logf!("[store] could not serialize {}: {e}", self.path.display()),
        }
    }
}

// ---------------------------------------------------------------------------
// Tracks and playlists
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Track {
    #[serde(default, deserialize_with = "de_string_lenient")]
    pub source: String,
    #[serde(default, deserialize_with = "de_string_lenient")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_string_lenient")]
    pub uri: Option<String>,
    #[serde(default, deserialize_with = "de_string_lenient")]
    pub title: String,
    #[serde(default, deserialize_with = "de_string_lenient")]
    pub artist: String,
    #[serde(default, deserialize_with = "de_f64_lenient")]
    pub duration: f64,
    #[serde(default, skip_serializing_if = "String::is_empty", deserialize_with = "de_string_lenient")]
    pub artwork: String,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_string_lenient")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_string_lenient")]
    pub playable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "de_opt_string_lenient")]
    pub uid: Option<String>,
}

impl Track {
    pub fn valid_source(&self) -> bool {
        matches!(self.source.as_str(), "spotify" | "soundcloud" | "youtube") && !self.id.is_empty()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Playlist {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub updated: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(default)]
    pub queue: Vec<Track>,
    #[serde(default)]
    pub current_uid: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Library {
    #[serde(default)]
    pub playlists: Vec<Playlist>,
}

// ---------------------------------------------------------------------------
// Settings and accounts
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default = "d_crossfade")]
    pub crossfade: f64,
    #[serde(default)]
    pub spotify_safe: bool,
    #[serde(default = "d_cache", rename = "cacheMB")]
    pub cache_mb: u64,
    #[serde(default = "d_ahead")]
    pub preload_ahead: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Where the audio engine runs: "auto" (inside MixDeck, moving to the default
    /// browser if Spotify's DRM fails), "app" (always inside), "browser" (always browser).
    #[serde(default = "d_player_host")]
    pub player_host: String,
    #[serde(default)]
    pub setup_done: bool,
    #[serde(default = "d_close_to_tray")]
    pub close_to_tray: bool,
    #[serde(default)]
    pub start_with_windows: bool,
}

fn d_crossfade() -> f64 { 8.0 }
fn d_cache() -> u64 { 1024 }
fn d_ahead() -> u32 { 2 }
fn d_player_host() -> String { "auto".into() }
/// Linux desktops don't always show tray icons (GNOME needs an extension), so closing
/// the window quits there unless the user turns this on.
fn d_close_to_tray() -> bool { !cfg!(target_os = "linux") }

impl Default for Settings {
    fn default() -> Self {
        Settings {
            crossfade: 8.0, spotify_safe: false, cache_mb: 1024, preload_ahead: 2, port: None,
            player_host: "auto".into(), setup_done: false, close_to_tray: d_close_to_tray(), start_with_windows: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SpotifyUser {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyConf {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub tokens: Option<Tokens>,
    #[serde(default)]
    pub user: Option<SpotifyUser>,
    /// Other linked accounts (the active one lives in `tokens`/`user`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<SpotifyAccount>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct SpotifyAccount {
    pub user: SpotifyUser,
    pub tokens: Tokens,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScAccount {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub profile: Option<Value>,
    #[serde(default)]
    pub oauth_token: String,
}

impl ScAccount {
    pub fn id(&self) -> String {
        match self.profile.as_ref().map(|p| &p["id"]) {
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            _ => self.username.clone(),
        }
    }
    pub fn name(&self) -> String {
        let p = self.profile.as_ref();
        let n = p.and_then(|p| p["username"].as_str()).or_else(|| p.and_then(|p| p["permalink"].as_str())).unwrap_or(&self.username);
        if n.is_empty() { "SoundCloud account".into() } else { n.to_string() }
    }
    pub fn is_empty(&self) -> bool { self.username.is_empty() && self.profile.is_none() && self.oauth_token.is_empty() }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ScConf {
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_id_at: i64,
    #[serde(default)]
    pub manual_client_id: String,
    #[serde(default)]
    pub oauth_token: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub profile: Option<Value>,
    /// Other linked accounts (the active one lives in the fields above).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub others: Vec<ScAccount>,
}

impl ScConf {
    pub fn active(&self) -> ScAccount {
        ScAccount { username: self.username.clone(), profile: self.profile.clone(), oauth_token: self.oauth_token.clone() }
    }
    pub fn set_active(&mut self, a: ScAccount) {
        self.username = a.username; self.profile = a.profile; self.oauth_token = a.oauth_token;
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub spotify: SpotifyConf,
    #[serde(default)]
    pub soundcloud: ScConf,
}
