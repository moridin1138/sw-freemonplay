#!/usr/bin/env bash
# Builds target/freemonplay.app: the host as a menu bar app.
#   ./build/macos/bundle.sh            build only
#   ./build/macos/bundle.sh --install  build and copy to /Applications (recommended)
#
# Run it from /Applications: macOS guards apps that live inside Documents, Desktop or
# Downloads, and can stall them behind "access files in your Documents folder" prompts.
#
# The app is ad-hoc signed. macOS ties Screen Recording and Accessibility permissions to
# the signature, so after rebuilding you may need to re-enable them in System Settings.
set -euo pipefail

cd "$(dirname "$0")/../.."
# rustup from Homebrew isn't on PATH by default.
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=target/freemonplay.app

cargo build --release -p freemonplay-host

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/freemonplay-host "$app/Contents/MacOS/freemonplay"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.forwardedu.freemonplay</string>
    <key>CFBundleName</key>
    <string>freemonplay</string>
    <key>CFBundleDisplayName</key>
    <string>freemonplay</string>
    <key>CFBundleExecutable</key>
    <string>freemonplay</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <!-- Menu bar only: no Dock icon or app menu. -->
    <key>LSUIElement</key>
    <true/>
    <!-- macOS 15 asks before an app talks to the local network (discovery and clients). -->
    <key>NSLocalNetworkUsageDescription</key>
    <string>freemonplay finds and streams to screens on your local network.</string>
    <key>NSBonjourServices</key>
    <array>
        <string>_freemonplay._udp</string>
    </array>
</dict>
</plist>
PLIST

codesign --force --sign - --identifier com.forwardedu.freemonplay "$app"
echo "Built $app"

if [[ "${1:-}" == "--install" ]]; then
    dest=/Applications/freemonplay.app
    if pgrep -xq freemonplay; then
        echo "Quitting the running freemonplay first"
        pkill -x freemonplay || true
        sleep 1
    fi
    rm -rf "$dest"
    cp -R "$app" "$dest"
    echo "Installed $dest. Open it from Launchpad or Spotlight."
fi
