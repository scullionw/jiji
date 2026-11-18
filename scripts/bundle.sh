#!/bin/sh
# Build Jiji.app into target/release/bundle.
#
#   scripts/bundle.sh             this machine's architecture, ad-hoc signed:
#                                 enough to run it here
#   JIJI_UNIVERSAL=1 …            arm64 and x86_64 in one binary (needs both
#                                 Rust targets installed)
#   JIJI_SIGN_IDENTITY="…" …      signed with that identity for distribution,
#                                 with the hardened runtime notarization needs
#
# Notarizing the bundle and packaging it (the DMG, the updater archive and
# its signature) belong to the release workflow, which has the keys:
# .github/workflows/release-macos.yml.
set -eu

cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
TARGET="${CARGO_TARGET_DIR:-target}"
RESOURCES="crates/jiji-desktop/resources"
APP="$TARGET/release/bundle/Jiji.app"

if [ "${JIJI_UNIVERSAL:-0}" = 1 ]; then
  cargo build --release -p jiji-desktop --target aarch64-apple-darwin
  cargo build --release -p jiji-desktop --target x86_64-apple-darwin
else
  cargo build --release -p jiji-desktop
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
if [ "${JIJI_UNIVERSAL:-0}" = 1 ]; then
  lipo -create -output "$APP/Contents/MacOS/jiji-desktop" \
    "$TARGET/aarch64-apple-darwin/release/jiji-desktop" \
    "$TARGET/x86_64-apple-darwin/release/jiji-desktop"
else
  cp "$TARGET/release/jiji-desktop" "$APP/Contents/MacOS/jiji-desktop"
fi
cp "$RESOURCES/icon.icns" "$APP/Contents/Resources/icon.icns"
cp "$RESOURCES/Info.plist" "$APP/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$VERSION" "$APP/Contents/Info.plist"

if [ -n "${JIJI_SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$JIJI_SIGN_IDENTITY" "$APP"
else
  codesign --force --sign - "$APP"
fi

echo "built $APP ($VERSION)"
