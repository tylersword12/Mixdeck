#!/usr/bin/env bash
# CI: sign the release APK with the key from the ANDROID_KEYSTORE_B64 /
# ANDROID_KEYSTORE_PASSWORD secrets. Tolerates stray spaces or line breaks from pasting.
set -euo pipefail
if [ -z "${KEYSTORE_B64:-}" ] || [ -z "${KEYSTORE_PASSWORD:-}" ]; then
  echo "::error title=Android signing key missing::Add the ANDROID_KEYSTORE_B64 and ANDROID_KEYSTORE_PASSWORD repository secrets (Settings > Secrets and variables > Actions)."
  exit 1
fi
KS="$RUNNER_TEMP/release.keystore"
printf '%s' "$KEYSTORE_B64" | tr -d ' \r\n\t' | base64 -d > "$KS" || { echo "ANDROID_KEYSTORE_B64 isn't valid base64. Copy the whole long line again."; exit 1; }
PASS="$(printf '%s' "$KEYSTORE_PASSWORD" | tr -d '\r\n' | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
echo "Keystore: $(wc -c < "$KS") bytes; password: ${#PASS} characters"
keytool -list -keystore "$KS" -storepass "$PASS" -alias mixdeck >/dev/null || { echo "The keystore and password don't match (or the alias isn't mixdeck). Check ANDROID_KEYSTORE_PASSWORD."; exit 1; }
mkdir -p out
APK=$(find src-tauri/gen/android/app/build/outputs/apk -name '*release*.apk' | head -n 1)
[ -n "$APK" ] || { echo "No release APK was built"; exit 1; }
echo "Unsigned APK: $APK"
BT=$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -n 1)
"$BT/zipalign" -f -p 4 "$APK" "$RUNNER_TEMP/aligned.apk"
"$BT/apksigner" sign --ks "$KS" --ks-pass "pass:$PASS" --ks-key-alias mixdeck \
  --v4-signing-enabled false --out out/MixDeck-Android.apk "$RUNNER_TEMP/aligned.apk"
"$BT/apksigner" verify out/MixDeck-Android.apk
ls -la out
