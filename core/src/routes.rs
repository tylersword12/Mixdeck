// HTTP routes: the API used by the player, the remote/app UI, and the setup wizard,
// plus the embedded web pages.
use crate::app::{App, Role};
use crate::cache::{cache_key, prepare_response, valid_key};
use crate::util::{num, s, truthy, ApiError, ApiResult};
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/../web"]
struct Web;

type S = State<Arc<App>>;
type Addr = ConnectInfo<SocketAddr>;

fn is_local(addr: &SocketAddr) -> bool { addr.ip().is_loopback() || addr.ip().to_canonical().is_loopback() }

fn require_local(app: &App, addr: &SocketAddr) -> ApiResult<()> {
    if is_local(addr) { Ok(()) } else {
        Err(ApiError::new(403, format!("This only works on the computer running MixDeck (http://127.0.0.1:{})", app.port())))
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/api/state", get(|State(app): S| async move { Json(app.public_state()) }))
        .route("/api/info", get(|State(app): S| async move { Json(app.info()) }))
        .route("/api/events", get(events))
        .route("/api/cmd", post(|State(app): S, Json(b): Json<Value>| async move { app.command(&b).map(Json) }))
        .route("/api/player/report", post(player_report))
        .route("/api/player/drm-failed", post(player_drm_failed))
        .route("/api/player/restart", post(player_restart))
        .route("/auth/spotify/login", get(spotify_login))
        .route("/auth/spotify/callback", get(spotify_callback))
        .route("/api/spotify/logout", post(spotify_logout))
        .route("/api/spotify/token", get(spotify_token))
        .route("/api/accounts/switch", post(account_switch))
        .route("/api/accounts/remove", post(account_remove))
        .route("/api/search", get(search))
        .route("/api/resolve", post(resolve))
        .route("/api/library/spotify/playlists", get(|State(app): S| async move { app.spotify_playlists().await.map(Json) }))
        .route("/api/library/spotify/playlist/{id}", get(|State(app): S, Path(id): Path<String>| async move {
            if !id.chars().all(|c| c.is_ascii_alphanumeric()) { return Err(ApiError::bad("bad id")); }
            app.spotify_playlist_tracks(&id).await.map(Json)
        }))
        .route("/api/library/spotify/liked", get(|State(app): S, Query(q): Query<HashMap<String, String>>| async move {
            app.spotify_liked(q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0)).await.map(Json)
        }))
        .route("/api/library/soundcloud/likes", get(|State(app): S, Query(q): Query<HashMap<String, String>>| async move {
            app.sc_likes(q.get("next").cloned()).await.map(Json)
        }))
        .route("/api/library/soundcloud/playlists", get(|State(app): S| async move { app.sc_playlists().await.map(Json) }))
        .route("/api/library/soundcloud/playlist/{id}", get(|State(app): S, Path(id): Path<String>| async move {
            if !id.chars().all(|c| c.is_ascii_digit()) { return Err(ApiError::bad("bad id")); }
            app.sc_playlist(&id).await.map(Json)
        }))
        .route("/api/playlists", get(playlists_list).post(playlists_create))
        .route("/api/playlists/{id}", get(playlist_get).put(playlist_update).delete(playlist_delete))
        .route("/api/prepare/{source}/{id}", post(|State(app): S, Path((source, id)): Path<(String, String)>| async move {
            let key = cache_key(&source, &id)?;
            let meta = app.cache_prepare(&key).await?;
            Ok::<_, ApiError>(Json(prepare_response(&key, &meta)))
        }))
        .route("/api/cache/status", get(|State(app): S| async move { Json(json!(app.cache.status.lock().clone())) }))
        .route("/api/cache/clear", post(|State(app): S| async move { app.cache_clear().await; Json(json!({})) }))
        .route("/api/cache", get(|State(app): S| async move { Json(json!({ "bytes": app.cache_size().await })) }))
        .route("/media/{key}/{file}", get(media))
        .route("/api/settings", post(settings))
        .route("/api/youtube/status", get(|State(app): S| async move { Json(app.tools_status().await) }))
        .route("/api/tools/status", get(|State(app): S| async move { Json(app.tools_status().await) }))
        .route("/api/tools/install", post(tools_install))
        .route("/api/youtube/update", post(|State(app): S, ConnectInfo(addr): Addr| async move {
            require_local(&app, &addr)?;
            app.tools_update().await.map(Json)
        }))
        .route("/api/soundcloud/refresh", post(|State(app): S| async move {
            let id = app.sc_client_id(true).await?;
            Ok::<_, ApiError>(Json(json!({ "ok": true, "clientId": format!("{}…", &id[..6.min(id.len())]) })))
        }))
        .route("/api/setup/done", post(|State(app): S, ConnectInfo(addr): Addr| async move {
            require_local(&app, &addr)?;
            { let mut c = app.config.lock(); c.data.settings.setup_done = true; c.save(); }
            app.broadcast_info();
            Ok::<_, ApiError>(Json(json!({})))
        }))
        .route("/api/open-url", post(open_url))
        .route("/api/import", post(import_old))
        .route("/api/logs", get(|| async { Json(json!({ "lines": crate::logger::recent() })) }))
        .route("/", get(index))
        .route("/player", get(|| async { asset("player.html") }))
        .route("/setup", get(|| async { asset("setup.html") }))
        .route("/{*path}", get(|Path(p): Path<String>| async move { asset(&p) }))
        .with_state(app)
}

// ---------------------------------------------------------------------------
// Static pages
// ---------------------------------------------------------------------------
fn asset(path: &str) -> Response {
    let path = path.trim_start_matches('/');
    match Web::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut ct = mime.essence_str().to_string();
            if ct.starts_with("text/") || ct == "application/javascript" { ct.push_str("; charset=utf-8"); }
            ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "no-cache".to_string())], f.data.into_owned()).into_response()
        }
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response(),
    }
}

async fn index(State(app): S, ConnectInfo(addr): Addr) -> Response {
    // The desktop app's first launch goes to the setup wizard.
    let needs_setup = app.host.is_app() && !app.config.lock().data.settings.setup_done;
    if needs_setup && is_local(&addr) { return Redirect::temporary("/setup").into_response(); }
    asset("index.html")
}

// ---------------------------------------------------------------------------
// Live updates (Server-Sent Events)
// ---------------------------------------------------------------------------
struct ClientGuard { app: Arc<App>, id: u64 }
impl Drop for ClientGuard {
    fn drop(&mut self) { self.app.remove_client(self.id); }
}

async fn events(State(app): S, ConnectInfo(addr): Addr, Query(q): Query<HashMap<String, String>>) -> Result<Sse<impl futures::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let role = if q.get("role").map(|r| r == "player").unwrap_or(false) { Role::Player } else { Role::Remote };
    if role == Role::Player { require_local(&app, &addr)?; }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    let id = app.add_client(role, tx.clone());
    let _ = tx.send(("state".into(), app.public_state().to_string()));
    app.broadcast_status();
    let guard = ClientGuard { app: app.clone(), id };
    let stream = async_stream::stream! {
        let _guard = guard;
        yield Ok(Event::default().retry(std::time::Duration::from_secs(2)).comment("hello"));
        while let Some((event, data)) = rx.recv().await {
            yield Ok(Event::default().event(event).data(data));
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(15))))
}

// ---------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------
async fn player_report(State(app): S, ConnectInfo(addr): Addr, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    app.player_report(&b);
    Ok(Json(json!({})))
}

/// The in-app player found that Spotify's DRM doesn't work there: move to the browser.
async fn player_drm_failed(State(app): S, ConnectInfo(addr): Addr, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    crate::logf!("[player] the in-app player can't play everything ({}); switching to the browser", s(&b["reason"]));
    let mode = app.config.lock().data.settings.player_host.clone();
    if mode != "auto" { return Ok(Json(json!({ "switched": false }))); }
    { let mut c = app.config.lock(); c.data.settings.player_host = "browser".into(); c.save(); }
    app.host.restart_player();
    app.broadcast_info();
    Ok(Json(json!({ "switched": true })))
}

async fn player_restart(State(app): S, ConnectInfo(addr): Addr) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    if app.host.is_app() {
        app.host.restart_player();
    } else {
        app.host.open_url(&app.player_url("tab"));
    }
    Ok(Json(json!({})))
}

// ---------------------------------------------------------------------------
// Spotify login
// ---------------------------------------------------------------------------
fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>{title}</title>
<body style="font-family:system-ui,Segoe UI,sans-serif;background:#0f0f14;color:#ececf1;display:grid;place-items:center;min-height:100vh;margin:0">
<div style="max-width:460px;padding:28px;background:#17171f;border:1px solid #2a2a36;border-radius:14px">{body}</div>"#))
}

async fn spotify_login(State(app): S, ConnectInfo(addr): Addr) -> Response {
    if !is_local(&addr) {
        return page("Log in from the host computer", &format!(
            "<h2>Log in from the computer running MixDeck</h2><p>Spotify only allows the login to finish at <b>http://127.0.0.1:{}</b>, so log in from MixDeck on that computer. You only need to do this once.</p>", app.port()
        )).into_response();
    }
    match app.spotify_login_url() {
        Ok(u) => Redirect::temporary(&u).into_response(),
        Err(e) => page("Spotify", &format!("<h2>Can't log in yet</h2><p>{}</p>", html_escape(&e.message))).into_response(),
    }
}

async fn spotify_callback(State(app): S, Query(q): Query<HashMap<String, String>>) -> Response {
    match app.spotify_callback(&q).await {
        Ok(()) => {
            app.broadcast_info();
            page("Spotify connected", "<h2 style=\"margin-top:0\">✓ Spotify connected</h2><p>You can close this tab and go back to MixDeck.</p><script>setTimeout(()=>window.close(),1500)</script>").into_response()
        }
        Err(e) => (StatusCode::BAD_REQUEST, page("Spotify login failed", &format!(
            "<h2 style=\"margin-top:0\">Spotify login failed</h2><p>{}</p><p>Check that <code>{}</code> is listed as a Redirect URI in your Spotify app, and that your Spotify account is added under User Management.</p>",
            html_escape(&e.message), app.spotify_redirect_uri()
        ))).into_response(),
    }
}

async fn spotify_logout(State(app): S, ConnectInfo(addr): Addr) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    app.spotify_logout();
    app.broadcast_info();
    Ok(Json(json!({})))
}

async fn spotify_token(State(app): S, ConnectInfo(addr): Addr) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    let (token, exp) = app.spotify_access_token().await?;
    Ok(Json(json!({ "token": token, "expiresAt": exp })))
}

async fn account_switch(State(app): S, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    app.account_switch(&s(&b["service"]), &s(&b["id"]))?;
    app.broadcast_info();
    Ok(Json(app.info()))
}

async fn account_remove(State(app): S, ConnectInfo(addr): Addr, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    app.account_remove(&s(&b["service"]), &s(&b["id"]))?;
    app.broadcast_info();
    Ok(Json(app.info()))
}

fn html_escape(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") }

// ---------------------------------------------------------------------------
// Search & links
// ---------------------------------------------------------------------------
async fn search(State(app): S, Query(q): Query<HashMap<String, String>>) -> ApiResult<Json<Value>> {
    let text = q.get("q").map(|s| s.trim().to_string()).unwrap_or_default();
    let offset: u64 = q.get("offset").and_then(|v| v.parse().ok()).unwrap_or(0);
    if text.is_empty() { return Ok(Json(json!({ "items": [] }))); }
    let r = match q.get("source").map(|s| s.as_str()) {
        Some("spotify") => app.spotify_search(&text, offset).await?,
        Some("youtube") => app.yt_search(&text, offset).await?,
        _ => app.sc_search(&text, offset).await?,
    };
    Ok(Json(r))
}

async fn resolve(State(app): S, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let url = s(&b["url"]);
    let l = url.to_lowercase();
    let r = if l.contains("spotify.com") || l.starts_with("spotify:") { app.spotify_resolve(&url).await? }
        else if l.contains("soundcloud.com") || l.contains("snd.sc") { app.sc_resolve(&url).await? }
        else if l.contains("youtube.com") || l.contains("youtu.be") || l.contains("youtube-nocookie.com") { app.yt_resolve(&url).await? }
        else { return Err(ApiError::bad("Paste a Spotify, SoundCloud or YouTube link")) };
    Ok(Json(r))
}

// ---------------------------------------------------------------------------
// MixDeck's own saved playlists
// ---------------------------------------------------------------------------
async fn playlists_list(State(app): S) -> Json<Value> {
    let lib = app.library.lock();
    Json(json!(lib.data.playlists.iter().map(|p| json!({ "id": p.id, "name": p.name, "count": p.tracks.len(), "updated": p.updated })).collect::<Vec<_>>()))
}

fn strip_uids(mut v: Vec<crate::store::Track>) -> Vec<crate::store::Track> { for t in &mut v { t.uid = None; } v }

async fn playlists_create(State(app): S, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let tracks = if truthy(&b["fromQueue"]) {
        app.session.lock().data.queue.clone()
    } else { App::clean_tracks(&b["tracks"])? };
    let name = s(&b["name"]);
    let p = crate::store::Playlist {
        id: crate::util::uid(),
        name: crate::util::clip(if name.is_empty() { "Untitled" } else { &name }, 100),
        tracks: strip_uids(tracks),
        updated: crate::util::now_ms(),
    };
    let id = p.id.clone();
    { let mut lib = app.library.lock(); lib.data.playlists.insert(0, p); lib.save(); }
    Ok(Json(json!({ "id": id })))
}

async fn playlist_get(State(app): S, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let lib = app.library.lock();
    let p = lib.data.playlists.iter().find(|p| p.id == id).ok_or_else(|| ApiError::not_found("Playlist not found"))?;
    Ok(Json(json!(p)))
}

async fn playlist_update(State(app): S, Path(id): Path<String>, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let queue = app.session.lock().data.queue.clone();
    let replace = if truthy(&b["fromQueue"]) { Some(strip_uids(queue)) }
        else if b["tracks"].is_array() { Some(strip_uids(App::clean_tracks(&b["tracks"])?)) }
        else { None };
    let append = if b["append"].is_array() { Some(strip_uids(App::clean_tracks(&b["append"])?)) } else { None };
    let mut lib = app.library.lock();
    let p = lib.data.playlists.iter_mut().find(|p| p.id == id).ok_or_else(|| ApiError::not_found("Playlist not found"))?;
    let name = s(&b["name"]);
    if !name.is_empty() { p.name = crate::util::clip(&name, 100); }
    if let Some(r) = replace { p.tracks = r; }
    if let Some(a) = append { p.tracks.extend(a); }
    p.updated = crate::util::now_ms();
    lib.save();
    Ok(Json(json!({})))
}

async fn playlist_delete(State(app): S, Path(id): Path<String>) -> Json<Value> {
    let mut lib = app.library.lock();
    lib.data.playlists.retain(|p| p.id != id);
    lib.save();
    Json(json!({}))
}

// ---------------------------------------------------------------------------
// Cached audio files (with Range support for seeking)
// ---------------------------------------------------------------------------
async fn media(State(app): S, Path((key, file)): Path<(String, String)>, headers: HeaderMap) -> Response {
    if !valid_key(&key) || file.is_empty() || !file.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') || file.starts_with('.') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = app.cache.dir.join(&key).join(&file);
    // Still downloading? Serve what's there and keep feeding the rest as it arrives.
    if let Some(rx) = app.cache_live(&key) {
        let p = rx.borrow().clone();
        if !p.done && p.error.is_none() {
            if p.kind == "file" && p.file == file {
                return serve_growing(path, rx, &headers).await;
            }
            // An HLS segment (or anything else) that isn't on disk yet: wait for it.
            wait_for_file(&path, rx).await;
        }
    }
    serve_static(&path, &file, &headers).await
}

fn content_type(file: &str) -> &'static str {
    match file.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
        "m3u8" => "application/vnd.apple.mpegurl", "ts" => "video/mp2t", "m4s" => "video/iso.segment",
        "mp4" | "m4a" => "audio/mp4", "aac" => "audio/aac", "mp3" => "audio/mpeg", "webm" => "audio/webm",
        "opus" | "ogg" => "audio/ogg", _ => "application/octet-stream",
    }
}

fn parse_range(headers: &HeaderMap) -> Option<(String, String)> {
    let r = headers.get(header::RANGE)?.to_str().ok()?.strip_prefix("bytes=")?.to_string();
    let (a, b) = r.split_once('-')?;
    Some((a.trim().to_string(), b.trim().to_string()))
}

/// Resolve a Range header against a known size: Ok((start, end)) or Err(()) if unsatisfiable.
fn resolve_range(range: &(String, String), size: u64) -> Result<(u64, u64), ()> {
    let (a, b) = range;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().unwrap_or(0);
        (size.saturating_sub(n), size.saturating_sub(1))
    } else {
        let start: u64 = a.parse().unwrap_or(0);
        let end: u64 = if b.is_empty() { size.saturating_sub(1) } else { b.parse::<u64>().unwrap_or(size.saturating_sub(1)).min(size.saturating_sub(1)) };
        (start, end)
    };
    if size == 0 || start >= size || start > end { Err(()) } else { Ok((start, end)) }
}

async fn wait_for_file(path: &std::path::Path, mut rx: tokio::sync::watch::Receiver<crate::cache::Progress>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
    loop {
        if tokio::fs::metadata(path).await.is_ok() { return; }
        {
            let p = rx.borrow_and_update();
            if p.done || p.error.is_some() { return; }
        }
        match tokio::time::timeout_at(deadline, rx.changed()).await {
            Ok(Ok(())) => continue,
            _ => return, // finished (channel closed) or gave up
        }
    }
}

async fn serve_static(path: &std::path::Path, file: &str, headers: &HeaderMap) -> Response {
    let Ok(meta) = tokio::fs::metadata(path).await else { return StatusCode::NOT_FOUND.into_response() };
    let size = meta.len();
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let Ok(mut f) = tokio::fs::File::open(path).await else { return StatusCode::NOT_FOUND.into_response() };
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(file)));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Some(range) = parse_range(headers) {
        let Ok((start, end)) = resolve_range(&range, size) else {
            h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes */{size}")).unwrap());
            return (StatusCode::RANGE_NOT_SATISFIABLE, h).into_response();
        };
        let len = end - start + 1;
        if f.seek(std::io::SeekFrom::Start(start)).await.is_err() { return StatusCode::INTERNAL_SERVER_ERROR.into_response(); }
        h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {start}-{end}/{size}")).unwrap());
        h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        return (StatusCode::PARTIAL_CONTENT, h, Body::from_stream(tokio_util::io::ReaderStream::new(f.take(len)))).into_response();
    }
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    (StatusCode::OK, h, Body::from_stream(tokio_util::io::ReaderStream::new(f))).into_response()
}

/// Stream a file that is still being downloaded: send bytes as they land on disk.
/// With a known final size this supports Range requests (so seeking works); without one
/// it's a plain stream from the start.
async fn serve_growing(path: std::path::PathBuf, rx: tokio::sync::watch::Receiver<crate::cache::Progress>, headers: &HeaderMap) -> Response {
    let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let expected = rx.borrow().expected;
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type(&file)));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    let (status, start, end) = match (expected, parse_range(headers)) {
        (Some(total), Some(range)) => match resolve_range(&range, total) {
            Ok((a, b)) => {
                h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
                h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes {a}-{b}/{total}")).unwrap());
                h.insert(header::CONTENT_LENGTH, HeaderValue::from(b - a + 1));
                (StatusCode::PARTIAL_CONTENT, a, Some(b))
            }
            Err(()) => {
                h.insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes */{total}")).unwrap());
                return (StatusCode::RANGE_NOT_SATISFIABLE, h).into_response();
            }
        },
        (Some(total), None) => {
            h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(total));
            (StatusCode::OK, 0, Some(total - 1))
        }
        (None, _) => (StatusCode::OK, 0, None), // size unknown: stream until the download ends
    };
    let body = async_stream::stream! {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut rx = rx;
        let mut f = match tokio::fs::File::open(&path).await {
            Ok(f) => f,
            Err(e) => { yield Err(e); return; }
        };
        if start > 0 { if let Err(e) = f.seek(std::io::SeekFrom::Start(start)).await { yield Err(e); return; } }
        let mut pos = start;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if let Some(end) = end { if pos > end { break; } }
            let (avail, finished, failed) = {
                let p = rx.borrow_and_update();
                (p.written, p.done, p.error.clone())
            };
            // Once the download is over, whatever is on disk is final.
            let avail = if finished || rx.has_changed().is_err() {
                tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(avail)
            } else { avail };
            if avail > pos {
                let mut want = (avail - pos).min(buf.len() as u64);
                if let Some(end) = end { want = want.min(end + 1 - pos); }
                match f.read(&mut buf[..want as usize]).await {
                    Ok(0) => {}
                    Ok(n) => { pos += n as u64; yield Ok(axum::body::Bytes::copy_from_slice(&buf[..n])); continue; }
                    Err(e) => { yield Err(e); return; }
                }
            }
            if let Some(err) = failed { yield Err(std::io::Error::other(err)); return; }
            if finished || rx.has_changed().is_err() {
                // Nothing more is coming.
                if end.map(|e| pos <= e).unwrap_or(false) { yield Err(std::io::Error::other("download ended early")); }
                return;
            }
            // Wait for more bytes (or the end of the download).
            match tokio::time::timeout(std::time::Duration::from_secs(60), rx.changed()).await {
                Ok(_) => {}
                Err(_) => { yield Err(std::io::Error::other("download stalled")); return; }
            }
        }
    };
    (status, h, Body::from_stream(body)).into_response()
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------
async fn settings(State(app): S, ConnectInfo(addr): Addr, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    let has = |k: &str| b.as_object().map(|o| o.contains_key(k)).unwrap_or(false);
    // Validate the SoundCloud profile first so a bad link doesn't half-apply the request.
    let profile_input = if has("scProfile") { Some(s(&b["scProfile"])) } else if has("scUsername") { Some(s(&b["scUsername"])) } else { None };
    let profile = match profile_input.as_deref().map(str::trim) {
        Some("") => Some(None),
        Some(input) => Some(Some(app.sc_find_profile(input).await.map_err(|e| e.with_status(400))?)),
        None => None,
    };
    let local_only = ["spotifyClientId", "scOauthToken", "scClientId", "playerHost", "startWithWindows", "closeToTray"];
    if local_only.iter().any(|k| has(k)) { require_local(&app, &addr)?; }
    if has("startWithWindows") {
        app.host.set_autostart(truthy(&b["startWithWindows"])).map_err(ApiError::bad)?;
    }
    let restart_player;
    {
        let mut cfg = app.config.lock();
        let c = &mut cfg.data;
        let st = &mut c.settings;
        if has("crossfade") { st.crossfade = num(&b["crossfade"], 0.0).clamp(0.0, 15.0); }
        if has("spotifySafe") { st.spotify_safe = truthy(&b["spotifySafe"]); }
        if has("cacheMB") { st.cache_mb = (num(&b["cacheMB"], 1024.0) as u64).clamp(100, 20000); }
        if has("preloadAhead") { st.preload_ahead = (num(&b["preloadAhead"], 2.0) as u32).clamp(1, 5); }
        if has("closeToTray") { st.close_to_tray = truthy(&b["closeToTray"]); }
        if has("startWithWindows") { st.start_with_windows = truthy(&b["startWithWindows"]); }
        restart_player = if has("playerHost") {
            let v = s(&b["playerHost"]);
            let v = if ["auto", "app", "browser"].contains(&v.as_str()) { v } else { "auto".into() };
            let changed = st.player_host != v;
            st.player_host = v;
            changed
        } else { false };
        if has("spotifyClientId") {
            let id = s(&b["spotifyClientId"]).trim().to_string();
            // Logins belong to a Client ID, so a new one means logging in again.
            if id != c.spotify.client_id { c.spotify.client_id = id; c.spotify.tokens = None; c.spotify.user = None; c.spotify.others.clear(); }
        }
        if let Some(p) = profile {
            match p {
                None => { c.soundcloud.username.clear(); c.soundcloud.profile = None; }
                Some(p) if truthy(&b["scAddAccount"]) => {
                    // Link another account: keep the current one in the list and make the new one active.
                    let new = crate::store::ScAccount { username: s(&p["permalink"]), profile: Some(p), oauth_token: String::new() };
                    let sc = &mut c.soundcloud;
                    let cur = sc.active();
                    sc.others.retain(|a| a.id() != new.id());
                    if !cur.is_empty() && cur.id() != new.id() { sc.others.insert(0, cur); }
                    sc.set_active(new);
                }
                Some(p) => { c.soundcloud.username = s(&p["permalink"]); c.soundcloud.profile = Some(p); }
            }
        }
        if has("scOauthToken") {
            let t = s(&b["scOauthToken"]).trim().to_string();
            c.soundcloud.oauth_token = regex::Regex::new(r"(?i)^OAuth\s+").unwrap().replace(&t, "").to_string();
        }
        if has("scClientId") { c.soundcloud.manual_client_id = s(&b["scClientId"]).trim().to_string(); }
        cfg.save();
    }
    if restart_player { app.host.restart_player(); }
    app.broadcast_state();
    app.broadcast_info();
    Ok(Json(app.info()))
}

async fn tools_install(State(app): S, ConnectInfo(addr): Addr) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    // Runs in the background; the wizard polls /api/tools/status for progress.
    let a = app.clone();
    tokio::spawn(async move { let _ = a.tools_install(false).await; });
    Ok(Json(json!({ "started": true })))
}

async fn open_url(State(app): S, ConnectInfo(addr): Addr, Json(b): Json<Value>) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    let mut url = s(&b["url"]);
    if url.starts_with('/') { url = format!("http://127.0.0.1:{}{url}", app.port()); }
    if !(url.starts_with("https://") || url.starts_with("http://")) { return Err(ApiError::bad("Only web links can be opened")); }
    app.host.open_url(&url);
    Ok(Json(json!({})))
}

/// Import settings, Spotify login and saved playlists from the earlier Node.js version.
async fn import_old(State(app): S, ConnectInfo(addr): Addr) -> ApiResult<Json<Value>> {
    require_local(&app, &addr)?;
    let host = app.host.clone();
    let picked = tokio::task::spawn_blocking(move || host.pick_folder()).await.map_err(|e| ApiError::internal(e.to_string()))?;
    let Some(dir) = picked else { return Ok(Json(json!({ "cancelled": true }))) };
    let candidates = [dir.join("data"), dir.clone()];
    let Some(src) = candidates.iter().find(|d| d.join("config.json").exists() || d.join("playlists.json").exists()) else {
        return Err(ApiError::bad("No MixDeck data found there. Pick the MixDeck folder (the one with start.bat) or its \"data\" folder."));
    };
    let mut imported = Vec::new();
    if let Ok(text) = std::fs::read_to_string(src.join("config.json")) {
        if let Ok(old) = serde_json::from_str::<crate::store::Config>(&text) {
            let mut cfg = app.config.lock();
            let c = &mut cfg.data;
            c.settings.crossfade = old.settings.crossfade;
            c.settings.spotify_safe = old.settings.spotify_safe;
            c.settings.cache_mb = old.settings.cache_mb;
            c.settings.preload_ahead = old.settings.preload_ahead;
            if !old.spotify.client_id.is_empty() { c.spotify = old.spotify; imported.push("Spotify login"); }
            if !old.soundcloud.username.is_empty() || old.soundcloud.profile.is_some() || !old.soundcloud.oauth_token.is_empty() {
                c.soundcloud.username = old.soundcloud.username;
                c.soundcloud.profile = old.soundcloud.profile;
                c.soundcloud.oauth_token = old.soundcloud.oauth_token;
                imported.push("SoundCloud profile");
            }
            imported.push("settings");
            cfg.save();
        }
    }
    let mut count = 0;
    if let Ok(text) = std::fs::read_to_string(src.join("playlists.json")) {
        if let Ok(old) = serde_json::from_str::<crate::store::Library>(&text) {
            let mut lib = app.library.lock();
            for p in old.playlists {
                if !lib.data.playlists.iter().any(|x| x.id == p.id) { lib.data.playlists.push(p); count += 1; }
            }
            lib.save();
        }
    }
    if let Ok(text) = std::fs::read_to_string(src.join("session.json")) {
        if let Ok(old) = serde_json::from_str::<crate::store::Session>(&text) {
            let mut sess = app.session.lock();
            if sess.data.queue.is_empty() && !old.queue.is_empty() { sess.data.queue = old.queue; sess.data.current_uid = None; sess.save(); imported.push("queue"); }
        }
    }
    crate::logf!("[import] from {}: {:?}, {count} playlists", src.display(), imported);
    app.broadcast_state();
    app.broadcast_info();
    Ok(Json(json!({ "imported": imported, "playlists": count, "from": src.display().to_string() })))
}
