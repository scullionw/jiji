# Releasing Jiji For macOS

Jiji ships outside the Mac App Store through GitHub Releases. The release workflow builds a macOS DMG for users and updater artifacts for Tauri's updater.

## Preflight

Run:

```bash
bun run release:preflight
```

The preflight checks version sync, updater configuration, required release files, GitHub secret names, and whether the GitHub repo is usable as a public release source.

## Required GitHub Secrets

The macOS release workflow expects:

```txt
APPLE_CERTIFICATE
APPLE_CERTIFICATE_PASSWORD
APPLE_API_ISSUER
APPLE_API_KEY
APPLE_API_PRIVATE_KEY
TAURI_SIGNING_PRIVATE_KEY
TAURI_SIGNING_PRIVATE_KEY_PASSWORD
```

`APPLE_CERTIFICATE` must be a base64-encoded Developer ID Application `.p12`. `APPLE_CERTIFICATE_PASSWORD` is the password used when exporting that `.p12`.

The updater private key and password are local maintainer secrets. The matching public key is committed in `src-tauri/tauri.conf.json`.

## Release Flow

Update versions together:

```txt
package.json
Cargo.toml
src-tauri/tauri.conf.json
```

Then verify, commit, tag, and push:

```bash
bun run release:preflight
bun run check
bun test
cargo test --workspace

git add .
git commit -m "release 0.1.1"
git tag app-v0.1.1
git push origin HEAD
git push origin app-v0.1.1
```

Pushing an `app-v*` tag starts `.github/workflows/release-macos.yml`. The workflow uses `--bundles app,dmg`; the `app` bundle is required so Tauri emits updater artifacts and signatures, and the `dmg` bundle is the user-facing download.

After the first release, confirm the exact generated DMG asset name in GitHub Releases before wiring a website download link.
