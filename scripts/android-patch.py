#!/usr/bin/env python3
"""Customise the Android project that `tauri android init` generates (src-tauri/gen/android).

Run after `tauri android init`:
  * installs MainActivity.kt / PlaybackService.kt from src-tauri/android-overlay
  * adds the permissions and foreground service MixDeck needs to AndroidManifest.xml
  * allows plain-HTTP to the built-in server (http://127.0.0.1) in release builds
"""
import json, pathlib, re, sys

root = pathlib.Path(__file__).resolve().parent.parent / "src-tauri"
conf = json.loads((root / "tauri.conf.json").read_text())
package = conf["identifier"]
gen = root / "gen" / "android"
app = gen / "app"
if not app.exists():
    sys.exit("gen/android not found: run `tauri android init` first")

# 1. Kotlin sources
src = app / "src" / "main" / "java" / pathlib.Path(*package.split("."))
src.mkdir(parents=True, exist_ok=True)
for kt in (root / "android-overlay").glob("*.kt"):
    (src / kt.name).write_text(kt.read_text().replace("__PACKAGE__", package))
    print("installed", src / kt.name)

# 2. Manifest
manifest = app / "src" / "main" / "AndroidManifest.xml"
m = manifest.read_text()
perms = [
    "android.permission.FOREGROUND_SERVICE",
    "android.permission.FOREGROUND_SERVICE_MEDIA_PLAYBACK",
    "android.permission.POST_NOTIFICATIONS",
    "android.permission.WAKE_LOCK",
    "android.permission.ACCESS_WIFI_STATE",
    "android.permission.ACCESS_NETWORK_STATE",
    "android.permission.INTERNET",
]
add = "".join(f'    <uses-permission android:name="{p}" />\n' for p in perms if p not in m)
m = m.replace("<application", add + "    <application", 1) if add else m
m = re.sub(r"\n    \s*<application", "\n    <application", m, count=1)
if "PlaybackService" not in m:
    m = m.replace("</application>", '        <service android:name=".PlaybackService" android:exported="false" android:foregroundServiceType="mediaPlayback" />\n    </application>', 1)
manifest.write_text(m)
print("patched", manifest)

# 3. Cleartext HTTP to the local server in release builds
for gradle in [app / "build.gradle.kts", app / "build.gradle"]:
    if gradle.exists():
        g = gradle.read_text()
        g2 = re.sub(r'(manifestPlaceholders\["usesCleartextTraffic"\]\s*=\s*)"false"', r'\1"true"', g)
        if g2 != g:
            gradle.write_text(g2)
            print("patched", gradle)
