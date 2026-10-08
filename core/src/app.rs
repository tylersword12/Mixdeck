// Shared application state: stores, live player status, connected screens (SSE), and the
// queue commands. Source-specific code lives in spotify.rs, soundcloud.rs, youtube.rs.
use crate::store::{Config, Library, Session, Store, Track};
use crate::util::{now_ms, num, s, truthy, uid, ApiError, ApiResult};
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// Things only the desktop shell can do (open windows, dialogs, autostart…).
/// The headless server binary uses a simple fallback implementation.
pub trait Host: Send + Sync + 'static {
    /// True when running inside the MixDeck desktop app.
    fn is_app(&self) -> bool { false }
    fn open_url(&self, url: &str) { let _ = open::that_detached(url); }
    /// Blocking native folder picker.
    fn pick_folder(&self) -> Option<PathBuf> { None }
    /// Start (or restart) the audio engine according to settings.playerHost.
    fn restart_player(&self) {}
    fn set_autostart(&self, _on: bool) -> Result<(), String> { Err("Only available in the desktop app".into()) }
    fn show_main(&self) {}
}

pub struct HeadlessHost;
impl Host for HeadlessHost {}

#[derive(Clone)]
pub struct Options {
    pub data_dir: PathBuf,
    pub port: u16,
    pub sc_web_base: String,
    pub sc_api_base: String,
    pub spotify_api_base: String,
    pub spotify_accounts_base: String,
    pub ytdlp_path: Option<PathBuf>,
    pub version: String,
}

impl Options {
    pub fn from_env(data_dir: PathBuf, port: u16) -> Self {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        Options {
            data_dir,
            port,
            sc_web_base: env("SC_WEB_BASE", "https://soundcloud.com"),
            sc_api_base: env("SC_API_BASE", "https://api-v2.soundcloud.com"),
            spotify_api_base: env("SPOTIFY_API_BASE", "https://api.spotify.com/v1"),
            spotify_accounts_base: env("SPOTIFY_ACCOUNTS_BASE", "https://accounts.spotify.com"),
            ytdlp_path: std::env::var("YTDLP_PATH").ok().map(PathBuf::from),
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Status {
    pub online: bool,
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub at: i64,
    pub message: String,
    pub transition: Value,
    pub next: Value,
    pub volume: Value,
    /// "app" or "browser": where the running player lives.
    pub host: Value,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            online: false, playing: false, position: 0.0, duration: 0.0, at: now_ms(), message: String::new(),
            transition: Value::Null, next: Value::Null, volume: json!(1), host: Value::Null,
        }
    }
}

#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum Role { Player, Remote }

pub struct Client {
    pub id: u64,
    pub role: Role,
    pub tx: UnboundedSender<(String, String)>,
}

pub struct App {
    pub opts: Options,
    pub http: reqwest::Client,
    pub config: Mutex<Store<Config>>,
    pub session: Mutex<Store<Session>>,
    pub library: Mutex<Store<Library>>,
    pub status: Mutex<Status>,
    pub clients: Mutex<Vec<Client>>,
    pub player_client: Mutex<Option<u64>>,
    next_client: AtomicU64,
    pub host: Arc<dyn Host>,
    // per-source state
    pub sp_pending: Mutex<HashMap<String, String>>, // oauth state -> PKCE verifier
    pub sp_refresh: tokio::sync::Mutex<()>,
    pub sc_cid_lock: tokio::sync::Mutex<()>,
    pub cache: crate::cache::CacheState,
    pub tools: crate::youtube::ToolsState,
    pub browser_child: Mutex<Option<std::process::Child>>,
}

impl App {
    pub fn new(opts: Options, host: Arc<dyn Host>) -> Arc<App> {
        std::fs::create_dir_all(&opts.data_dir).ok();
        let config = Store::<Config>::load(&opts.data_dir, "config");
        let session = Store::<Session>::load(&opts.data_dir, "session");
        let library = Store::<Library>::load(&opts.data_dir, "playlists");
        let http = reqwest::Client::builder()
            .user_agent(crate::soundcloud::UA)
            .connect_timeout(std::time::Duration::from_secs(15))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("http client");
        Arc::new(App {
            http,
            config: Mutex::new(config),
            session: Mutex::new(session),
            library: Mutex::new(library),
            status: Mutex::new(Status::default()),
            clients: Mutex::new(Vec::new()),
            player_client: Mutex::new(None),
            next_client: AtomicU64::new(1),
            host,
            sp_pending: Mutex::new(HashMap::new()),
            sp_refresh: tokio::sync::Mutex::new(()),
            sc_cid_lock: tokio::sync::Mutex::new(()),
            cache: crate::cache::CacheState::new(&opts.data_dir),
            tools: crate::youtube::ToolsState::default(),
            browser_child: Mutex::new(None),
            opts,
        })
    }

    pub fn data_dir(&self) -> &std::path::Path { &self.opts.data_dir }
    pub fn port(&self) -> u16 { self.opts.port }

    // ---- live updates ------------------------------------------------------

    pub fn add_client(&self, role: Role, tx: UnboundedSender<(String, String)>) -> u64 {
        let id = self.next_client.fetch_add(1, Ordering::SeqCst);
        if role == Role::Player {
            let mut pc = self.player_client.lock();
            if let Some(old) = *pc {
                // Only one player at a time; tell the old one to go idle.
                self.send_to(old, "command", &json!({ "type": "replaced" }));
            }
            *pc = Some(id);
            self.status.lock().online = true;
        }
        self.clients.lock().push(Client { id, role, tx });
        id
    }

    pub fn remove_client(&self, id: u64) {
        self.clients.lock().retain(|c| c.id != id);
        let was_player = {
            let mut pc = self.player_client.lock();
            if *pc == Some(id) { *pc = None; true } else { false }
        };
        if was_player {
            {
                let mut st = self.status.lock();
                st.online = false;
                st.playing = false;
                st.transition = Value::Null;
            }
            self.broadcast_status();
        }
    }

    pub fn player_online(&self) -> bool { self.player_client.lock().is_some() }

    fn send_to(&self, id: u64, event: &str, data: &Value) {
        let msg = (event.to_string(), data.to_string());
        if let Some(c) = self.clients.lock().iter().find(|c| c.id == id) {
            let _ = c.tx.send(msg);
        }
    }

    pub fn broadcast(&self, event: &str, data: &Value) {
        let text = data.to_string();
        self.clients.lock().retain(|c| c.tx.send((event.to_string(), text.clone())).is_ok());
    }

    pub fn broadcast_state(&self) { let st = self.public_state(); self.broadcast("state", &st); }
    pub fn broadcast_status(&self) { let st = serde_json::to_value(&*self.status.lock()).unwrap_or_default(); self.broadcast("status", &st); }
    pub fn broadcast_info(&self) { let i = self.info(); self.broadcast("info", &i); }

    pub fn public_state(&self) -> Value {
        let sess = self.session.lock();
        json!({
            "queue": sess.data.queue,
            "currentUid": sess.data.current_uid,
            "status": *self.status.lock(),
            "settings": self.config.lock().data.settings,
        })
    }

    pub fn to_player(&self, cmd: Value) -> ApiResult<()> {
        let id = *self.player_client.lock();
        match id {
            Some(id) => { self.send_to(id, "command", &cmd); Ok(()) }
            None => Err(ApiError::new(409, if self.host.is_app() {
                "The player isn't running. Use \"Restart player\" in MixDeck.".to_string()
            } else {
                format!("The host player is not open. Open http://127.0.0.1:{}/player on the host computer.", self.port())
            })),
        }
    }

    fn queue_changed(&self) {
        self.session.lock().save();
        self.broadcast_state();
    }

    pub fn lan_urls(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(list) = local_ip_address::list_afinet_netifas() {
            for (_name, ip) in list {
                if let std::net::IpAddr::V4(v4) = ip {
                    if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() { continue; }
                    let u = format!("http://{v4}:{}/", self.port());
                    if !out.contains(&u) { out.push(u); }
                }
            }
        }
        // Typical home networks first.
        out.sort_by_key(|u| if u.contains("//192.168.") { 0 } else if u.contains("//10.") { 1 } else { 2 });
        out
    }

    pub fn info(&self) -> Value {
        let cfg = self.config.lock();
        let c = &cfg.data;
        json!({
            "port": self.port(),
            "version": self.opts.version,
            "app": self.host.is_app(),
            "dataDir": self.data_dir().display().to_string(),
            "lanUrls": self.lan_urls(),
            "playerOnline": self.player_online(),
            "defaultBrowser": crate::browser::default_browser().map(|b| b.name),
            "spotify": {
                "clientId": c.spotify.client_id,
                "loggedIn": c.spotify.tokens.as_ref().map(|t| !t.refresh_token.is_empty()).unwrap_or(false),
                "user": c.spotify.user,
                "redirectUri": self.spotify_redirect_uri(),
            },
            "soundcloud": {
                "username": c.soundcloud.username,
                "profile": c.soundcloud.profile,
                "hasToken": !c.soundcloud.oauth_token.is_empty(),
                "manualClientId": c.soundcloud.manual_client_id,
                "clientIdFound": !c.soundcloud.client_id.is_empty(),
            },
            "settings": c.settings,
        })
    }

    // ---- queue commands ----------------------------------------------------

    fn clean_track(t: &Value) -> ApiResult<Track> {
        let mut tr: Track = serde_json::from_value(t.clone()).map_err(|_| ApiError::bad("Bad track"))?;
        if !tr.valid_source() { return Err(ApiError::bad("Bad track")); }
        tr.uid = Some(uid());
        Ok(tr)
    }

    pub fn clean_tracks(v: &Value) -> ApiResult<Vec<Track>> {
        v.as_array().map(|a| a.iter().map(Self::clean_track).collect()).unwrap_or(Ok(vec![]))
    }

    pub fn command(&self, body: &Value) -> ApiResult {
        let action = s(&body["action"]);
        match action.as_str() {
            "add" => {
                let tracks = Self::clean_tracks(&body["tracks"])?;
                let n = tracks.len();
                let first = tracks.first().and_then(|t| t.uid.clone());
                let where_ = s(&body["where"]);
                let (has_current, _) = {
                    let mut sess = self.session.lock();
                    let cur = sess.data.current_uid.clone();
                    let q = &mut sess.data.queue;
                    let ci = cur.as_ref().and_then(|u| q.iter().position(|t| t.uid.as_ref() == Some(u)));
                    let at = if where_ == "end" || ci.is_none() { q.len() } else { ci.unwrap() + 1 };
                    q.splice(at..at, tracks);
                    (cur.is_some(), ())
                };
                self.queue_changed();
                if let Some(first) = first {
                    if where_ == "now" {
                        self.to_player(json!({ "type": "play", "uid": first }))?;
                    } else if !has_current && self.player_online() && !self.status.lock().playing && truthy(&body["autostart"]) {
                        self.to_player(json!({ "type": "play", "uid": first }))?;
                    }
                }
                Ok(json!({ "added": n }))
            }
            "replace" => {
                let tracks = Self::clean_tracks(&body["tracks"])?;
                let first = tracks.first().and_then(|t| t.uid.clone());
                {
                    let mut sess = self.session.lock();
                    sess.data.queue = tracks;
                    sess.data.current_uid = None;
                }
                self.queue_changed();
                if truthy(&body["play"]) {
                    if let Some(f) = first { self.to_player(json!({ "type": "play", "uid": f }))?; }
                }
                Ok(json!({}))
            }
            "remove" => {
                let target = s(&body["uid"]);
                let (was_current, next_uid) = {
                    let mut sess = self.session.lock();
                    let Some(i) = sess.data.queue.iter().position(|t| t.uid.as_deref() == Some(target.as_str())) else { return Ok(json!({})) };
                    sess.data.queue.remove(i);
                    let was_current = sess.data.current_uid.as_deref() == Some(target.as_str());
                    if was_current { sess.data.current_uid = None; }
                    (was_current, sess.data.queue.get(i).and_then(|t| t.uid.clone()))
                };
                self.queue_changed();
                // Removing the track that's playing moves on to whatever took its place.
                if was_current && self.player_online() {
                    let cmd = match next_uid { Some(u) => json!({ "type": "play", "uid": u }), None => json!({ "type": "stop", "clear": true }) };
                    let _ = self.to_player(cmd);
                }
                Ok(json!({}))
            }
            "move" => {
                let target = s(&body["uid"]);
                {
                    let mut sess = self.session.lock();
                    let cur = sess.data.current_uid.clone();
                    let q = &mut sess.data.queue;
                    let Some(i) = q.iter().position(|t| t.uid.as_deref() == Some(target.as_str())) else { return Ok(json!({})) };
                    let t = q.remove(i);
                    let to = if s(&body["to"]) == "next" {
                        cur.and_then(|u| q.iter().position(|x| x.uid.as_ref() == Some(&u))).map(|p| p as f64 + 1.0).unwrap_or(0.0)
                    } else {
                        num(&body["to"], 0.0)
                    };
                    let to = (to.max(0.0) as usize).min(q.len());
                    q.insert(to, t);
                }
                self.queue_changed();
                Ok(json!({}))
            }
            "shuffle" => {
                use rand::seq::SliceRandom;
                {
                    let mut sess = self.session.lock();
                    let cur = sess.data.current_uid.clone();
                    let q = &mut sess.data.queue;
                    let start = cur.and_then(|u| q.iter().position(|t| t.uid.as_ref() == Some(&u))).map(|i| i + 1).unwrap_or(0);
                    q[start..].shuffle(&mut rand::thread_rng());
                }
                self.queue_changed();
                Ok(json!({}))
            }
            "clear" => {
                let stop = {
                    let mut sess = self.session.lock();
                    let cur = sess.data.current_uid.clone();
                    let keep = if truthy(&body["keepCurrent"]) {
                        cur.and_then(|u| sess.data.queue.iter().find(|t| t.uid.as_ref() == Some(&u)).cloned())
                    } else { None };
                    sess.data.queue = keep.into_iter().collect();
                    let stop = sess.data.queue.is_empty();
                    if stop { sess.data.current_uid = None; }
                    stop
                };
                self.queue_changed();
                if stop && self.player_online() { let _ = self.to_player(json!({ "type": "stop", "clear": true })); }
                Ok(json!({}))
            }
            "play" => {
                let mut u = s(&body["uid"]);
                if u.is_empty() {
                    u = self.session.lock().data.queue.first().and_then(|t| t.uid.clone()).unwrap_or_default();
                }
                self.to_player(json!({ "type": "play", "uid": u }))?;
                Ok(json!({}))
            }
            "pause" | "resume" | "toggle" | "next" | "prev" | "stop" => {
                self.to_player(json!({ "type": action }))?;
                Ok(json!({}))
            }
            "seek" => { self.to_player(json!({ "type": "seek", "ms": num(&body["ms"], 0.0) }))?; Ok(json!({})) }
            "volume" => {
                let v = num(&body["value"], 1.0).clamp(0.0, 1.0);
                self.to_player(json!({ "type": "volume", "value": v }))?;
                Ok(json!({}))
            }
            _ => Err(ApiError::bad("Unknown action")),
        }
    }

    /// Status report from the player page (~1/second and on every change).
    pub fn player_report(&self, b: &Value) {
        if let Some(obj) = b.as_object() {
            if obj.contains_key("currentUid") {
                let new = match &b["currentUid"] { Value::Null => None, v => Some(s(v)) };
                let changed = {
                    let mut sess = self.session.lock();
                    if sess.data.current_uid != new { sess.data.current_uid = new; sess.save(); true } else { false }
                };
                if changed { self.broadcast_state(); }
            }
            let mut st = self.status.lock();
            if let Some(v) = obj.get("playing") { st.playing = truthy(v); }
            if let Some(v) = obj.get("position") { st.position = num(v, 0.0); }
            if let Some(v) = obj.get("duration") { st.duration = num(v, 0.0); }
            if let Some(v) = obj.get("message") { st.message = s(v); }
            if let Some(v) = obj.get("transition") { st.transition = v.clone(); }
            if let Some(v) = obj.get("next") { st.next = v.clone(); }
            if let Some(v) = obj.get("volume") { st.volume = v.clone(); }
            if let Some(v) = obj.get("host") { st.host = v.clone(); }
            st.at = now_ms();
            st.online = true;
        }
        self.broadcast_status();
    }

    pub fn save_all(&self) {
        self.session.lock().save();
        self.config.lock().save();
        self.library.lock().save();
    }

    /// Stop the external browser player, if one was started.
    pub fn kill_browser_player(&self) {
        if let Some(mut c) = self.browser_child.lock().take() {
            let _ = c.kill();
        }
    }
}
