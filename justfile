# Build the release app bundle and install it into /Applications
install:
    scripts/bundle.sh
    rm -rf /Applications/Jiji.app
    ditto target/release/bundle/Jiji.app /Applications/Jiji.app
    @echo "Installed Jiji to /Applications"
