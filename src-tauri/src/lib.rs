// MixDeck app shell for Windows, Linux and Android.
//
// Desktop: starts the MixDeck server, opens the main window, keeps a hidden player window
// for the audio engine, and lives in the system tray.
// Android: starts the same server inside the app; the main page hosts the player itself
// (see remote.js) and a foreground service keeps music going with the screen off.

use mixdeck_core::{App, Host, Options};
use once_cell::sync::OnceCell;
use std::path::PathBuf;
use std::sync::{Arc, Weak};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;
#[cfg(desktop)]
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
#[cfg(desktop)]
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
#[cfg(desktop)]
use tauri::WindowEvent;
#[cfg(desktop)]
use tauri_plugin_autostart::ManagerExt as _;
#[cfg(desktop)]
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

/// Flags for the built-in Windows web view: the defaults Tauri uses, plus permission to
/// play audio without a click and no throttling when the window is hidden.
#[cfg(desktop)]
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
    --autoplay-policy=no-user-gesture-required --disable-background-timer-throttling \
    --disable-renderer-backgrounding --disable-backgrounding-occluded-windows";

struct TauriHost {
    handle: OnceCell<AppHandle>,
    core: OnceCell<Weak<App>>,
}

impl TauriHost {
    fn core(&self) -> Option<Arc<App>> { self.core.get().and_then(|w| w.upgrade()) }
}

impl Host for TauriHost {
    fn is_app(&self) -> bool { true }

    fn open_url(&self, url: &str) {
        match self.handle.get() {
            Some(h) => { if let Err(e) = h.opener().open_url(url, None::<&str>) { mixdeck_core::logf!("[open] {url}: {e}"); } }
            None => { let _ = mixdeck_core::util_open(url); }
        }
    }

    #[cfg(desktop)]
    fn pick_folder(&self) -> Option<PathBuf> {
        let h = self.handle.get()?;
        h.dialog().file().set_title("Choose your old MixDeck folder").blocking_pick_folder().and_then(|p| p.into_path().ok())
    }

    fn restart_player(&self) {
        if let (Some(h), Some(core)) = (self.handle.get().cloned(), self.core()) {
            // Window work has to happen on the main thread.
            let _ = h.clone().run_on_main_thread(move || start_player(&h, &core));
        }
    }

    #[cfg(desktop)]
    fn set_autostart(&self, on: bool) -> Result<(), String> {
        let h = self.handle.get().ok_or("not ready")?;
        let al = h.autolaunch();
        if on { al.enable() } else { al.disable() }.map_err(|e| format!("Couldn't change the startup setting: {e}"))
    }

    fn show_main(&self) {
        #[cfg(desktop)]
        if let Some(h) = self.handle.get() { show_main(h); }
    }
}

/// Open a link outside MixDeck (default browser).
fn mixdeck_open(h: &AppHandle, url: &str) {
    if let Err(e) = h.opener().open_url(url, None::<&str>) { mixdeck_core::logf!("[open] {url}: {e}"); }
}

fn base_url(core: &App) -> String { format!("http://127.0.0.1:{}", core.port()) }

/// Start the audio engine where settings say: hidden window inside MixDeck, or the browser.
#[cfg(desktop)]
fn start_player(h: &AppHandle, core: &Arc<App>) {
    let mode = core.config.lock().data.settings.player_host.clone();
    if mode == "browser" {
        if let Some(w) = h.get_webview_window("player") { let _ = w.destroy(); }
        if let Err(e) = core.launch_browser_player() {
            mixdeck_core::logf!("[player] couldn't open the browser player: {e}");
        }
        return;
    }
    core.kill_browser_player();
    let url = format!("{}/player?auto=1&host=app", base_url(core));
    if let Some(w) = h.get_webview_window("player") {
        if let Ok(u) = url.parse() { let _ = w.navigate(u); }
        return;
    }
    let Ok(u) = url.parse() else { return };
    let h2 = h.clone();
    let built = WebviewWindowBuilder::new(h, "player", WebviewUrl::External(u))
        .title("MixDeck Player")
        .inner_size(480.0, 640.0)
        .visible(false)
        .skip_taskbar(true)
        .additional_browser_args(BROWSER_ARGS)
        .on_new_window(move |u, _| { mixdeck_open(&h2, u.as_str()); NewWindowResponse::Deny })
        .build();
    match built {
        Ok(w) => {
            // The player window is never closed by the user; hide instead of destroying.
            let w2 = w.clone();
            w.on_window_event(move |e| if let WindowEvent::CloseRequested { api, .. } = e { api.prevent_close(); let _ = w2.hide(); });
        }
        Err(e) => mixdeck_core::logf!("[player] couldn't create the player window: {e}"),
    }
}

/// Android: the main page hosts the player in a hidden frame. "Restarting" reloads that
/// frame, or hands the player to Chrome when settings say so (Spotify DRM fallback).
#[cfg(mobile)]
fn start_player(h: &AppHandle, core: &Arc<App>) {
    let mode = core.config.lock().data.settings.player_host.clone();
    if mode == "browser" {
        mixdeck_open(h, &core.player_url("browser"));
    }
    core.broadcast("player-restart", &serde_json::json!({ "mode": mode }));
}

#[cfg(desktop)]
fn show_main(h: &AppHandle) {
    if let Some(w) = h.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

#[cfg(desktop)]
fn create_main(h: &AppHandle, core: &Arc<App>, visible: bool) -> tauri::Result<()> {
    let url = format!("{}/", base_url(core)).parse().expect("url");
    let w = WebviewWindowBuilder::new(h, "main", WebviewUrl::External(url))
        .title("MixDeck")
        .inner_size(1180.0, 800.0)
        .min_inner_size(380.0, 560.0)
        .center()
        .visible(visible)
        .additional_browser_args(BROWSER_ARGS)
        .on_new_window({ let h2 = h.clone(); move |u, _| { mixdeck_open(&h2, u.as_str()); NewWindowResponse::Deny } })
        .build()?;
    let core2 = Arc::downgrade(core);
    let w2 = w.clone();
    w.on_window_event(move |e| {
        if let WindowEvent::CloseRequested { api, .. } = e {
            let to_tray = core2.upgrade().map(|c| c.config.lock().data.settings.close_to_tray).unwrap_or(true);
            if to_tray {
                // Keep playing in the background; the tray icon brings the window back.
                api.prevent_close();
                let _ = w2.hide();
            } else {
                quit(w2.app_handle());
            }
        }
    });
    Ok(())
}

#[cfg(desktop)]
fn quit(h: &AppHandle) {
    if let Some(core) = h.try_state::<Arc<App>>() {
        core.save_all();
        core.kill_browser_player();
    }
    h.exit(0);
}

#[cfg(desktop)]
fn tray_command(h: &AppHandle, action: &str) {
    if let Some(core) = h.try_state::<Arc<App>>() {
        if let Err(e) = core.command(&serde_json::json!({ "action": action })) {
            mixdeck_core::logf!("[tray] {action}: {e}");
        }
    }
}

#[cfg(desktop)]
fn build_tray(h: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(h, "show", "Open MixDeck", true, None::<&str>)?;
    let toggle = MenuItem::with_id(h, "toggle", "Play / Pause", true, None::<&str>)?;
    let next = MenuItem::with_id(h, "next", "Next track", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(h)?;
    let quit_i = MenuItem::with_id(h, "quit", "Quit MixDeck", true, None::<&str>)?;
    let menu = Menu::with_items(h, &[&show, &toggle, &next, &sep, &quit_i])?;
    let mut tb = TrayIconBuilder::with_id("mixdeck")
        .tooltip("MixDeck")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|h, e| match e.id().as_ref() {
            "show" => show_main(h),
            "toggle" => tray_command(h, "toggle"),
            "next" => tray_command(h, "next"),
            "quit" => quit(h),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = h.default_window_icon() { tb = tb.icon(icon.clone()); }
    tb.build(h)?;
    Ok(())
}

/// Android: the single app window shows MixDeck's interface from the built-in server.
#[cfg(mobile)]
fn create_main(h: &AppHandle, core: &Arc<App>) -> tauri::Result<()> {
    let url = format!("{}/", base_url(core)).parse().expect("url");
    let h2 = h.clone();
    WebviewWindowBuilder::new(h, "main", WebviewUrl::External(url))
        .on_new_window(move |u, _| { mixdeck_open(&h2, u.as_str()); NewWindowResponse::Deny })
        .build()?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let host = Arc::new(TauriHost { handle: OnceCell::new(), core: OnceCell::new() });
    let host_for_setup = host.clone();
    #[cfg(desktop)]
    let started_minimized = std::env::args().any(|a| a == "--minimized");

    let builder = tauri::Builder::default();
    #[cfg(desktop)]
    let builder = builder
        .plugin(tauri_plugin_single_instance::init(|h, _args, _cwd| show_main(h)))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_dialog::init());
    builder
        .plugin(tauri_plugin_opener::init())
        .setup(move |app| {
            let h = app.handle().clone();
            let _ = host_for_setup.handle.set(h.clone());
            #[cfg(desktop)]
            let data_dir = mixdeck_core::default_data_dir();
            #[cfg(mobile)]
            let data_dir = app.path().app_data_dir()?;
            let port = std::env::var("MIXDECK_PORT").ok().and_then(|p| p.parse().ok())
                .or_else(|| peek_port(&data_dir))
                .unwrap_or(8787);
            let opts = Options::from_env(data_dir, port);
            let core = match tauri::async_runtime::block_on(mixdeck_core::start(opts, host_for_setup.clone())) {
                Ok(c) => c,
                Err(e) => {
                    #[cfg(desktop)]
                    h.dialog().message(format!("MixDeck couldn't start.\n\n{e}\n\nIf another copy of MixDeck is running, close it from the tray and try again."))
                        .title("MixDeck").kind(MessageDialogKind::Error).blocking_show();
                    mixdeck_core::logf!("[app] couldn't start: {e}");
                    std::process::exit(1);
                }
            };
            let _ = host_for_setup.core.set(Arc::downgrade(&core));
            app.manage(core.clone());
            #[cfg(desktop)]
            {
                build_tray(&h)?;
                create_main(&h, &core, !started_minimized)?;
                start_player(&h, &core);
            }
            #[cfg(mobile)]
            create_main(&h, &core)?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building MixDeck")
        .run(|_h, _e| {
            #[cfg(desktop)]
            if let tauri::RunEvent::ExitRequested { api, code, .. } = _e {
                // Closing windows shouldn't quit; only "Quit MixDeck" (which passes a code) does.
                if code.is_none() { api.prevent_exit(); }
            }
        });
    drop(host);
}

/// Read a custom port from config.json before the server starts (rarely used).
fn peek_port(data_dir: &std::path::Path) -> Option<u16> {
    let text = std::fs::read_to_string(data_dir.join("config.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["settings"]["port"].as_u64().map(|p| p as u16)
}
