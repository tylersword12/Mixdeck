# Android signing key

Every MixDeck APK is signed with this key, so new versions install over old ones.
This repository is private, so the key is kept here for simplicity.

If you ever make the repository public, move the key into GitHub secrets instead:
1. Settings → Secrets and variables → Actions → New repository secret
   - `ANDROID_KEYSTORE_B64`: the output of `base64 -w0 mixdeck.keystore`
   - `ANDROID_KEYSTORE_PASSWORD`: the contents of `password.txt`
2. Delete this folder from the repository.

The build uses the secrets automatically when they exist.
