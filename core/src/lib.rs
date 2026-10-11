//! MixDeck core: the local server that powers the desktop app, the phone remote and the
//! audio player page. The desktop shell (src-tauri) embeds this; `mixdeck-server` runs it
//! headless for development.
pub mod accounts;
pub mod app;
pub mod browser;
pub mod cache;
pub mod logger;
pub mod routes;
pub mod soundcloud;
pub mod spotify;
pub mod store;
pub mod updates;
pub mod util;
pub mod youtube;
#[cfg(feature = "native-youtube")]
pub mod ytnative;

pub use app::{App, HeadlessHost, Host, Options};
use std::net::SocketAddr;
use std::sync::Arc;

/// Bind the server and start serving in the background. Returns the shared app state.
pub async fn start(opts: Options, host: Arc<dyn Host>) -> anyhow::Result<Arc<App>> {
    logger::init(&opts.data_dir);
    let app = App::new(opts, host);
    let addr = SocketAddr::from(([0, 0, 0, 0], app.port()));
    let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            anyhow::anyhow!("Port {} is already in use. Is MixDeck already running?", app.port())
        } else { anyhow::anyhow!(e) }
    })?;
    let router = routes::router(app.clone());
    logf!("MixDeck {} listening on port {}", app.opts.version, app.port());
    for u in app.lan_urls() { logf!("  phones: {u}"); }
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await {
            logf!("[server] stopped: {e}");
        }
    });
    let a = app.clone();
    tokio::spawn(async move { a.cache_prune().await; });
    // Look for a newer build shortly after starting.
    let a = app.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let on = a.config.lock().data.settings.check_updates;
        if on && updates::current_build() > 0 { a.check_updates().await; }
    });
    Ok(app)
}

/// Where MixDeck keeps its data: %APPDATA%\\MixDeck on Windows, ~/.local/share/mixdeck on
/// Linux (an existing ~/.mixdeck is kept), ~/Library/Application Support/MixDeck on macOS.
/// The Android app passes its own private folder instead.
pub fn default_data_dir() -> std::path::PathBuf {
    use std::path::PathBuf;
    if let Ok(d) = std::env::var("MIXDECK_DATA") { return d.into(); }
    #[cfg(windows)]
    if let Ok(a) = std::env::var("APPDATA") { return PathBuf::from(a).join("MixDeck"); }
    if let Ok(h) = std::env::var("HOME") {
        let home = PathBuf::from(h);
        if cfg!(target_os = "macos") { return home.join("Library/Application Support/MixDeck"); }
        let legacy = home.join(".mixdeck");
        if legacy.exists() { return legacy; }
        let base = std::env::var("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|_| home.join(".local/share"));
        return base.join("mixdeck");
    }
    PathBuf::from("mixdeck-data")
}

/// Open a link in the user's default browser.
pub fn util_open(url: &str) -> std::io::Result<()> { open::that_detached(url) }

/// Bytes of a single-file download needed before playback can start.
pub fn cache_start_bytes() -> u64 { 96 * 1024 }
