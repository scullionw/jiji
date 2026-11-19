# Releasing Jiji for macOS

Jiji ships outside the Mac App Store through GitHub Releases. Pushing an `app-v*` tag starts `.github/workflows/release-macos.yml`, which builds a universal `Jiji.app`, signs and notarizes it, and publishes:

| Asset | |
|---|---|
| `Jiji_darwin_universal.dmg` | The download. `https://github.com/scullionw/jiji/releases/latest/download/Jiji_darwin_universal.dmg` always serves the newest; the README and the website link to it. |
| `Jiji_darwin_universal.app.tar.gz` and `.sig` | What an installed copy downloads to update itself, and its signature. |
| `latest.json` | The updater's manifest: the version, the notes, and where the archive is. |

The app binary is `jiji-desktop`, and it embeds the `jiji` command line from `crates/jiji-cli`: the `jiji` entry point the app installs forwards to it, so the DMG carries the CLI without a second executable.

## Release flow

1. Set the version in `Cargo.toml` (`[workspace.package]`); every crate inherits it.
2. Give the version its own `## <version> - <date>` section in `CHANGELOG.md`. The workflow publishes that section as the release notes (and the notes the updater shows), and fails a tag that has none.
3. Verify, commit, tag and push:

```bash
cargo test --workspace
scripts/release-preflight app-v0.5.0
open target/release/bundle/Jiji.app

git commit -am "release 0.5.0"
git tag app-v0.5.0
git push origin HEAD app-v0.5.0
```

The workflow refuses a tag that does not match the workspace version.

## Preflight

`scripts/release-preflight [tag]` checks a release before its tag is pushed, so a mistake the workflow would only find after the push (or not at all) shows up here first. With no tag it checks `app-v<workspace version>`. It reports each check as ok, a warning or a blocker, and exits non-zero on any blocker:

- the tag matches the workspace version, and does not already tag another commit (a version that has shipped);
- `CHANGELOG.md` has a section for the version, with notes in it;
- the update feed in `crates/jiji-desktop/src/host/update.rs` is the latest release of the repository `origin` points at;
- both Rust targets are installed (`rustup target add x86_64-apple-darwin` on an Apple Silicon Mac);
- the bundle builds, universal as the workflow builds it (this Mac's slice alone, with a blocker, when a target is missing), into `target/release/bundle/Jiji.app`;
- its `Info.plist` passes `plutil -lint` and names the version, each slice is built for the macOS floor the bundle declares, and `codesign --verify --deep --strict` passes;
- with `gh` signed in: every secret the workflow names is set (names only; an unset `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` is a warning, since an updater key may have no password), and the repository is public, which downloads and update checks need. Without `gh`, or signed out of it, these are skipped with a note.

It publishes, tags and changes nothing; the build is the only thing it writes, and `CARGO_TARGET_DIR` moves it.

## Secrets

```txt
APPLE_CERTIFICATE
APPLE_CERTIFICATE_PASSWORD
APPLE_API_ISSUER
APPLE_API_KEY
APPLE_API_PRIVATE_KEY
TAURI_SIGNING_PRIVATE_KEY
TAURI_SIGNING_PRIVATE_KEY_PASSWORD
```

`APPLE_CERTIFICATE` is a base64-encoded Developer ID Application `.p12`, and `APPLE_CERTIFICATE_PASSWORD` the password it was exported with. The three `APPLE_API_*` secrets are an App Store Connect API key, for notarization.

The `TAURI_SIGNING_*` pair is the updater key. Jiji's first releases were built with Tauri, and every installed copy verifies an update against that key's public half, so the key and its format stay: it is a minisign key in the Tauri updater's wrapping, the workflow signs with Tauri's signer, and `latest.json` and the archive keep the layout that updater reads. The public key is `PUBKEY` in `crates/jiji-desktop/src/host/update.rs`. Losing the private key means no installed copy can update.
