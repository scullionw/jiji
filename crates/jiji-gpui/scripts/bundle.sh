#!/bin/sh
# Build Jiji.app from the GPUI crate.
#
#   scripts/bundle.sh             a side-by-side build: "Jiji GPUI.app",
#                                 its own bundle id, safe to keep next to
#                                 an installed Jiji. It never updates
#                                 itself: the release feed carries the
#                                 shipping app, which would replace it.
#   JIJI_BUNDLE_RELEASE=1 …       the shipping identity (Jiji.app,
#                                 com.lauf.jiji), with the in-app updater,
#                                 plus the updater archive
#
# The bundle is ad-hoc signed, which is enough to run it on this machine.
# Notarization, and the minisign signature the in-app updater verifies,
# belong to the release pipeline (they need its keys).
set -eu

cd "$(dirname "$0")/.."
ROOT=$(pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

if [ "${JIJI_BUNDLE_RELEASE:-0}" = 1 ]; then
  NAME="Jiji"
  IDENTIFIER="com.lauf.jiji"
  FEATURES="self-update"
else
  NAME="Jiji GPUI"
  IDENTIFIER="com.lauf.jiji.gpui"
  FEATURES=""
fi

cargo build --release --features "$FEATURES"

OUT="$ROOT/target/release/bundle"
APP="$OUT/$NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$ROOT/target/release/jiji-desktop" "$APP/Contents/MacOS/jiji-desktop"
cp "$ROOT/../../src-tauri/icons/icon.icns" "$APP/Contents/Resources/icon.icns"

# Folders are openable documents so Launch Services accepts them for this
# app: that is what lets the `jiji` launcher's `open -b <id> <dir>` (and a
# folder dropped on the Dock icon) deliver the path as an open event.
# "Alternate" claims the type without becoming Finder's default for folders.
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>$NAME</string>
	<key>CFBundleDisplayName</key>
	<string>$NAME</string>
	<key>CFBundleIdentifier</key>
	<string>$IDENTIFIER</string>
	<key>CFBundleExecutable</key>
	<string>jiji-desktop</string>
	<key>CFBundleIconFile</key>
	<string>icon.icns</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSMinimumSystemVersion</key>
	<string>11.0</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>Folder</string>
			<key>CFBundleTypeRole</key>
			<string>Viewer</string>
			<key>LSHandlerRank</key>
			<string>Alternate</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>public.folder</string>
			</array>
		</dict>
	</array>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP" >/dev/null 2>&1
echo "built $APP ($VERSION, $IDENTIFIER)"

if [ "${JIJI_BUNDLE_RELEASE:-0}" = 1 ]; then
  # What the updater downloads: the bundle as a gzipped tarball. The
  # release pipeline signs it with minisign and lists it in latest.json.
  ARCHIVE="$OUT/$NAME.app.tar.gz"
  rm -f "$ARCHIVE"
  tar -czf "$ARCHIVE" -C "$OUT" "$NAME.app"
  echo "built $ARCHIVE (unsigned)"
fi
