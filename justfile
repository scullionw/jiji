# Skips the DMG and updater artifacts (those need the release signing key)
# Build the release app bundle and install it into /Applications
install:
    bun tauri build --bundles app --config '{"bundle":{"createUpdaterArtifacts":false}}'
    rm -rf /Applications/Jiji.app
    ditto target/release/bundle/macos/Jiji.app /Applications/Jiji.app
    @echo "Installed Jiji $(jq -r .version src-tauri/tauri.conf.json) to /Applications"
