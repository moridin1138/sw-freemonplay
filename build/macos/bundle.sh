#!/usr/bin/env bash
# Builds "target/Freemonplay Host.app": the host as a menu bar app.
#   ./build/macos/bundle.sh              build only
#   ./build/macos/bundle.sh --install    build and copy to /Applications (recommended)
#   ./build/macos/bundle.sh --universal  Apple Silicon + Intel in one binary (used for releases)
#   ./build/macos/bundle.sh --client     the client instead: "Freemonplay Screen.app", for
#                                        Macs used as screens (combines with the others)
#
# Run it from /Applications: macOS guards apps that live inside Documents, Desktop or
# Downloads, and can stall them behind "access files in your Documents folder" prompts.
#
# Signing: macOS ties Screen Recording and Accessibility permissions to the signature. With
# the "freemonplay Local Signing" identity (create it once with make-signing-identity.sh)
# they survive rebuilds; with the ad-hoc fallback you re-enable them after each rebuild.
# Releases set FMP_SIGN_IDENTITY to a Developer ID Application identity: signed with the
# hardened runtime and a secure timestamp, ready for notarize.sh.
set -euo pipefail

cd "$(dirname "$0")/../.."
# rustup from Homebrew isn't on PATH by default.
export PATH="$HOME/.cargo/bin:/opt/homebrew/opt/rustup/bin:$PATH"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

install=false
universal=false
client=false
for arg in "$@"; do
    case "$arg" in
        --install) install=true ;;
        --universal) universal=true ;;
        --client) client=true ;;
        *) echo "unknown option $arg" >&2; exit 1 ;;
    esac
done

if $client; then
    package=freemonplay-screen
    name="Freemonplay Screen"
    old_name="freemonplay Screen"
    executable=freemonplay-screen
    bundle_id=io.github.moridin1138.freemonplay.screen
    # An ordinary app with a Dock icon and windows.
    ui_element=false
    network_reason="Freemonplay Screen finds computers to be a screen for on your local network."
else
    package=freemonplay-host
    name="Freemonplay Host"
    old_name=Freemonplay
    executable=freemonplay-host
    bundle_id=io.github.moridin1138.freemonplay
    # Menu bar only: no Dock icon or app menu.
    ui_element=true
    network_reason="Freemonplay Host finds and streams to screens on your local network."
fi
app="target/$name.app"

if $universal; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    cargo build --release --locked -p "$package" --target aarch64-apple-darwin --target x86_64-apple-darwin
    mkdir -p target/universal
    lipo -create -output "target/universal/$package" \
        "target/aarch64-apple-darwin/release/$package" "target/x86_64-apple-darwin/release/$package"
    binary="target/universal/$package"
else
    cargo build --release -p "$package"
    binary="target/release/$package"
fi

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/$executable"
cp build/macos/icon/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>${bundle_id}</string>
    <key>CFBundleName</key>
    <string>${name}</string>
    <key>CFBundleDisplayName</key>
    <string>${name}</string>
    <key>CFBundleExecutable</key>
    <string>${executable}</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>LSUIElement</key>
    <${ui_element}/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <!-- macOS 15 asks before an app talks to the local network (discovery, streaming). -->
    <key>NSLocalNetworkUsageDescription</key>
    <string>${network_reason}</string>
    <key>NSBonjourServices</key>
    <array>
        <string>_freemonplay._udp</string>
    </array>
</dict>
</plist>
PLIST

identity="freemonplay Local Signing"
if [ -n "${FMP_SIGN_IDENTITY:-}" ]; then
    codesign --force --options runtime --timestamp --sign "$FMP_SIGN_IDENTITY" --identifier "$bundle_id" "$app"
    echo "Signed with \"$FMP_SIGN_IDENTITY\" (hardened runtime)"
elif security find-identity -p codesigning 2>/dev/null | grep -q "\"$identity\""; then
    codesign --force --sign "$identity" --identifier "$bundle_id" "$app"
    echo "Signed with \"$identity\""
else
    codesign --force --sign - --identifier "$bundle_id" "$app"
    echo "Ad-hoc signed. Permissions reset on each rebuild; run build/macos/make-signing-identity.sh once to fix that."
fi
echo "Built $app"
# Keep the build copy out of Launch Services, so Spotlight, Launchpad and the Dock only ever
# find the installed app (two registered copies with the same ID show up as question marks).
/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -u "$app" 2>/dev/null || true

if $install; then
    dest="/Applications/$name.app"
    # Before 0.3 the apps were "freemonplay.app" (running as "freemonplay") and
    # "freemonplay Screen.app".
    for running in "$executable" freemonplay freemonplay-client; do
        if pgrep -xq "$running"; then
            echo "Quitting the running $running first"
            pkill -x "$running" || true
            sleep 1
        fi
    done
    rm -rf "$dest" "/Applications/$old_name.app"
    cp -R "$app" "$dest"
    # Leave only one copy, so Spotlight and Launchpad can't open the build copy by mistake
    # (permissions and the Documents-folder prompts make that copy misbehave).
    rm -rf "$app"
    echo "Installed $dest. Open it from Launchpad or Spotlight."
fi
