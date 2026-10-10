# MixDeck

One playlist across **Spotify**, **SoundCloud** and **YouTube**, with smooth crossfades. Runs on **Windows**, **Linux** and **Android**. On a computer, any phone on your Wi-Fi can be the remote.

## Install

Download from this repository's **Releases** page (the `latest` release is rebuilt on every change):

| Platform | File | Notes |
|---|---|---|
| Windows 10/11 | `MixDeck-Setup.exe` | SmartScreen may say "Windows protected your PC": **More info → Run anyway** (the installer isn't code-signed). |
| Ubuntu, Debian, Mint, Pop!_OS | `MixDeck-linux-amd64.deb` | `sudo apt install ./MixDeck-linux-amd64.deb` (pulls in the GStreamer codecs it needs). |
| Fedora, openSUSE | `MixDeck-linux-x86_64.rpm` | `sudo dnf install ./MixDeck-linux-x86_64.rpm` |
| Any other Linux | `MixDeck-linux-x86_64.AppImage` | `chmod +x MixDeck-linux-x86_64.AppImage` then run it. |
| Android 8+ | `MixDeck-Android.apk` | Open it on the phone and allow installing from that app when asked. |

A setup wizard walks you through Spotify, SoundCloud, YouTube and (on computers) connecting your phone as a remote.

## How it works

- **The app** (Tauri + Rust) runs a small local server on port 8787. Its window and your phone's browser use the same interface.
- **The player** is a hidden page inside MixDeck that plays the audio and does the crossfades. Spotify's player needs browser DRM (Widevine). If that doesn't work inside the app, MixDeck moves the player to your default browser: a separate small window with its own profile. You can change this in Settings → Player.
- **SoundCloud** uses the website's own internal API, the same way NewPipe does (`core/src/soundcloud.rs`).
- **YouTube** on Windows and Linux uses [yt-dlp](https://github.com/yt-dlp/yt-dlp), which MixDeck downloads along with a JavaScript runtime. On Android, where yt-dlp can't run, MixDeck has a built-in YouTube client ([RustyPipe](https://codeberg.org/ThetaDev/rustypipe), NewPipe's approach in Rust).
- SoundCloud and YouTube tracks are downloaded ahead of time, and start playing while the download is still arriving, so crossfades never stall.
- **Spotify** uses the official Web API and Web Playback SDK with your own developer app (Premium required).
- **Your data** (settings, Spotify login, playlists, download cache, tools) lives in `%APPDATA%\MixDeck` on Windows, `~/.local/share/mixdeck` on Linux, and the app's private storage on Android.

### Android

The phone is the player: MixDeck runs the server and the audio engine inside the app and keeps playing with the screen off (a "MixDeck is playing" notification stays up while it does). Other devices on the same Wi-Fi can still use it as a remote. Android WebView usually can't play Spotify's protected audio, so when a Spotify track comes up MixDeck may hand the player over to Chrome; SoundCloud and YouTube play inside the app.

### Linux

Closing the window quits MixDeck by default, because GNOME hides tray icons unless the AppIndicator extension is installed. Turn on **Settings → Keep playing in the tray when I close the window** if your desktop shows them (KDE, Cinnamon, XFCE, or GNOME with the extension).

## Building

Every push to `main` is built by GitHub Actions (`.github/workflows/build.yml`) for Windows, Linux and Android, and published as the `latest` release. Tagging `v2.1.0` publishes a versioned release.

Locally, install [Rust](https://rustup.rs) plus the [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS, then:

```
npx @tauri-apps/cli@^2 build                 # Windows or Linux
npx @tauri-apps/cli@^2 android init          # Android, first time
python3 scripts/android-patch.py             # adds the background-playback service
npx @tauri-apps/cli@^2 android build --apk
```

To work on the server without the app shell (any OS):

```
cargo run -p mixdeck-core --bin mixdeck-server -- --data ./data
```

Then open http://127.0.0.1:8787/player (click Start) and http://127.0.0.1:8787/.

## Layout

```
core/            Rust server: API, Spotify, SoundCloud, YouTube, media cache, player fallback
  src/routes.rs      HTTP API and embedded web pages
  src/spotify.rs     PKCE login, Web API (Feb 2026 Development Mode rules)
  src/soundcloud.rs  client_id discovery and internal API (NewPipe approach)
  src/youtube.rs     yt-dlp + JS runtime install, search, downloads (Windows/Linux)
  src/ytnative.rs    built-in YouTube client (Android)
  src/cache.rs       ahead-of-time audio downloads that can play while arriving
  src/browser.rs     default-browser detection and the browser player fallback
src-tauri/       App shell: windows, tray, autostart, installer and Android config
  android-overlay/   Android activity + background playback service
  android-signing/   APK signing key (see its README)
scripts/         android-patch.py: wires the overlay into the generated Android project
web/             Interface: remote/app UI, player (crossfade engine), setup wizard
```

## Notes

- Spotify's Developer Policy forbids overlapping Spotify audio with other audio. Settings → **Spotify-safe transitions** turns the overlap off for transitions that involve Spotify.
- Using SoundCloud's and YouTube's website APIs is against their terms of service. Keep MixDeck for personal use.
