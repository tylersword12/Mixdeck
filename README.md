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

### Updates

MixDeck checks this repository's `latest` release when it starts (Settings → Updates; you can turn it off there). On Windows and Linux, closing MixDeck with an update available asks whether to install it. Windows installs silently, the AppImage replaces itself, and .deb/.rpm installs ask for your password. Android asks when the app opens and downloads the new APK in your browser; tap it to install. You can also update any time from Settings → Updates.

### Accounts

You can link several Spotify and SoundCloud accounts and switch between them in Settings, or from the account picker at the top of the Library. Each extra Spotify account must be added under User Management in your Spotify developer app (and have Premium).

## How it works

- **The app** (Tauri + Rust) runs a small local server on port 8787. Its window and your phone's browser use the same interface.
- **The player** is a hidden page inside MixDeck that plays the audio and does the crossfades. Spotify's player needs browser DRM (Widevine). If that doesn't work inside the app, MixDeck moves the player to your default browser: a separate small window with its own profile. You can change this in Settings → Player.
- **SoundCloud** uses the website's own internal API, the same way NewPipe does (`core/src/soundcloud.rs`).
- **YouTube** audio uses the same workaround NewPipe adopted in 2026 after YouTube began enforcing its SABR streaming protocol: it asks for streams as YouTube's visionOS (Apple Vision Pro) app, whose links play without YouTube's JavaScript (`core/src/visionos.rs`). On Windows and Linux, [yt-dlp](https://github.com/yt-dlp/yt-dlp) is tried first (MixDeck downloads it with a JavaScript runtime and updates it daily), with the visionOS client as the fallback. On Android, where yt-dlp can't run, the visionOS client comes first, then [RustyPipe](https://codeberg.org/ThetaDev/rustypipe)'s clients; RustyPipe also handles YouTube search and playlists there.
- SoundCloud and YouTube tracks are downloaded ahead of time, and start playing while the download is still arriving, so crossfades never stall.
- **Spotify** uses the official Web API and Web Playback SDK with your own developer app (Premium required).
- **Your data** (settings, Spotify login, playlists, download cache, tools) lives in `%APPDATA%\MixDeck` on Windows, `~/.local/share/mixdeck` on Linux, and the app's private storage on Android.

### Android

The phone is the player: MixDeck runs the server and the audio engine inside the app and keeps playing with the screen off (a "MixDeck is playing" notification stays up while it does). Other devices on the same Wi-Fi can still use it as a remote. Android WebView usually can't play Spotify's protected audio, so when a Spotify track comes up MixDeck may hand the player over to Chrome; SoundCloud and YouTube play inside the app.

### Linux

Closing the window quits MixDeck by default, because GNOME hides tray icons unless the AppIndicator extension is installed. Turn on **Settings → Keep playing in the tray when I close the window** if your desktop shows them (KDE, Cinnamon, XFCE, or GNOME with the extension).

## Building

Every push to `main` is built by GitHub Actions (`.github/workflows/build.yml`) for Windows, Linux and Android, and published as the `latest` release with a `version.json` that installed copies check. Builds are versioned `2.1.<run number>`. Tagging `v2.1.0` publishes a versioned release.

The Android build is signed with a key kept in two repository secrets, `ANDROID_KEYSTORE_B64` (the keystore, base64) and `ANDROID_KEYSTORE_PASSWORD`. Keep a copy of the key somewhere safe: Android only installs updates signed with the same key.

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
  src/ytnative.rs    built-in YouTube search/playlists/download (Android)
  src/visionos.rs    NewPipe's visionOS-client workaround for YouTube audio (all platforms)
  src/cache.rs       ahead-of-time audio downloads that can play while arriving
  src/browser.rs     default-browser detection and the browser player fallback
  src/accounts.rs    several linked Spotify/SoundCloud accounts
  src/updates.rs     update check and install from GitHub releases
src-tauri/       App shell: windows, tray, autostart, installer and Android config
  android-overlay/   Android activity + background playback service
scripts/         android-patch.py (wires the overlay into the Android project), stamp-version.js
web/             Interface: remote/app UI, player (crossfade engine), setup wizard
```

## Notes

- Spotify's Developer Policy forbids overlapping Spotify audio with other audio. Settings → **Spotify-safe transitions** turns the overlap off for transitions that involve Spotify.
- Using SoundCloud's and YouTube's website APIs is against their terms of service. Keep MixDeck for personal use.
