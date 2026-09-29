# Releasing Jiji For macOS

Jiji ships outside the Mac App Store through GitHub Releases. The release workflow builds a macOS DMG for users and updater artifacts for Tauri's updater.

## Preflight

Run:

```bash
bun run release:preflight
```

The preflight checks version sync, updater configuration, required release files, GitHub secret names, and whether the GitHub repo is usable as a public release source.

The desktop executable is `jiji-desktop`; it embeds the same command-line
implementation as the standalone `jiji` binary from `crates/jiji-cli`. This
keeps Cargo workspace builds from overwriting one executable with the other.
The app-installed `jiji` entry point forwards commands to the embedded CLI,
so the existing app/DMG release includes the command-line tools without a
separate sidecar. Check `cargo run -p jiji-cli -- --help` and
`target/debug/jiji-desktop --help` when validating CLI packaging locally.

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

Give the version its own `## <version> - <date>` section in `CHANGELOG.md`.
The workflow publishes that section as the GitHub release notes (and the
updater's notes), and fails a tag that has none.

Then verify, commit, tag, and push:

```bash
bun run release:preflight
bun run check
bun run test
cargo test --workspace

git add .
git commit -m "release 0.1.1"
git tag app-v0.1.1
git push origin HEAD
git push origin app-v0.1.1
```

Pushing an `app-v*` tag starts `.github/workflows/release-macos.yml`. The workflow uses `--bundles app,dmg`; the `app` bundle is required so Tauri emits updater artifacts and signatures, and the `dmg` bundle is the user-facing download.

The DMG is published as `Jiji_darwin_universal.dmg`, so
`https://github.com/scullionw/jiji/releases/latest/download/Jiji_darwin_universal.dmg`
always serves the newest release; the README and the website link to it.
