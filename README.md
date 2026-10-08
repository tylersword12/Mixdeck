# MixDeck

One playlist across **Spotify**, **SoundCloud** and **YouTube**, played on your Windows PC with smooth crossfades and controlled from any phone on your Wi-Fi.

## Install

Download **MixDeck-Setup.exe** from this repository's **Releases** page and run it. A setup wizard walks you through Spotify, SoundCloud, YouTube and connecting your phone.

The installer isn't code-signed, so Windows SmartScreen may say "Windows protected your PC". Click **More info → Run anyway**.

## How it works

- **The app** (Tauri + Rust) runs a small local server on port 8787. Its window and your phone both use the same interface.
- **The player** is a hidden window inside MixDeck that plays the audio and does the crossfades. Spotify's player needs browser DRM (Widevine). If that doesn't work inside the app window, MixDeck moves the player to your default browser: a separate small window with its own profile. You can change this in Settings → Player.
- **SoundCloud** uses the website's own internal API, the same way NewPipe does (`core/src/soundcloud.rs`). **YouTube** uses [yt-dlp](https://github.com/yt-dlp/yt-dlp), which MixDeck downloads along with a JavaScript runtime (your Node.js 22+ or Deno). SoundCloud and YouTube tracks are downloaded ahead of time so crossfades never stall.
- **Spotify** uses the official Web API and Web Playback SDK with your own developer app (Premium required).
- **Your data** (settings, Spotify login, playlists, download cache, tools) lives in `%APPDATA%\MixDeck`.

## Building

Every push to `main` is built by GitHub Actions on Windows (`.github/workflows/build.yml`) and published as the `latest` release. Tagging `v2.1.0` publishes a versioned release.

To build locally on Windows: install [Rust](https://rustup.rs) and the Visual Studio C++ Build Tools, then run:

```
npx @tauri-apps/cli@^2 build
```

To work on the server without the desktop shell (any OS):

```
cargo run -p mixdeck-core --bin mixdeck-server -- --data ./data
```

Then open http://127.0.0.1:8787/player (click Start) and http://127.0.0.1:8787/.

## Layout

```
core/            Rust server: API, Spotify, SoundCloud, YouTube (yt-dlp), media cache, player fallback
  src/routes.rs      HTTP API and embedded web pages
  src/spotify.rs     PKCE login, Web API (Feb 2026 Development Mode rules)
  src/soundcloud.rs  client_id discovery and internal API (NewPipe approach)
  src/youtube.rs     yt-dlp + JS runtime install, search, downloads
  src/cache.rs       ahead-of-time audio downloads (HLS and files)
  src/browser.rs     default-browser detection and the browser player fallback
src-tauri/       Desktop shell: windows, tray, autostart, installer config
web/             Interface: remote/app UI, player (crossfade engine), setup wizard
```

## Notes

- Spotify's Developer Policy forbids overlapping Spotify audio with other audio. Settings → **Spotify-safe transitions** turns the overlap off for transitions that involve Spotify.
- Using SoundCloud's and YouTube's website APIs is against their terms of service. Keep MixDeck for personal use.
