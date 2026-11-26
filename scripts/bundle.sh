#!/bin/sh
# Build Jiji.app into target/release/bundle.
#
#   scripts/bundle.sh             for this Mac, ad-hoc signed: enough to run
#                                 it here
#   JIJI_TARGET=<rust target> …   for that target instead of this Mac's (the
#                                 release names aarch64-apple-darwin, whatever
#                                 it is built on)
#   JIJI_SIGN_IDENTITY="…" …      signed with that identity for distribution,
#                                 with the hardened runtime notarization needs
#
# The bundle holds one architecture. Jiji ships for Apple Silicon; a build
# for another architecture would be its own bundle and its own download,
# never a second half of this one.
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

# Built for the oldest macOS the bundle says it runs on, so the system
# turns away the same Macs the binary would fail on.
MACOSX_DEPLOYMENT_TARGET=$(plutil -extract LSMinimumSystemVersion raw "$RESOURCES/Info.plist")
export MACOSX_DEPLOYMENT_TARGET

if [ -n "${JIJI_TARGET:-}" ]; then
  cargo build --release -p jiji-desktop --target "$JIJI_TARGET"
  BINARY="$TARGET/$JIJI_TARGET/release/jiji-desktop"
else
  cargo build --release -p jiji-desktop
  BINARY="$TARGET/release/jiji-desktop"
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/jiji-desktop"
# Drop the map to this build's object files: it only resolves on the Mac
# that built it. Function names stay, so a crash log still reads.
strip -S "$APP/Contents/MacOS/jiji-desktop"
cp "$RESOURCES/icon.icns" "$APP/Contents/Resources/icon.icns"
cp "$RESOURCES/Info.plist" "$APP/Contents/Info.plist"
plutil -replace CFBundleShortVersionString -string "$VERSION" "$APP/Contents/Info.plist"
plutil -replace CFBundleVersion -string "$VERSION" "$APP/Contents/Info.plist"

if [ -n "${JIJI_SIGN_IDENTITY:-}" ]; then
  codesign --force --options runtime --timestamp --sign "$JIJI_SIGN_IDENTITY" "$APP"
else
  codesign --force --sign - "$APP"
fi

echo "built $APP ($VERSION, $(lipo -archs "$APP/Contents/MacOS/jiji-desktop"))"
