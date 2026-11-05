#!/bin/bash
# Build an iPad app with the shared Rust screen client. No signing for the simulator.
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export IPHONEOS_DEPLOYMENT_TARGET=16.0
platform="${1:-simulator}"
case "$platform" in
    simulator) target=aarch64-apple-ios-sim; sdk=iphonesimulator; triple=arm64-apple-ios16.0-simulator ;;
    device) target=aarch64-apple-ios; sdk=iphoneos; triple=arm64-apple-ios16.0 ;;
    *) echo "Usage: $0 [simulator|device]" >&2; exit 1 ;;
esac
rustup target add "$target"
cargo build -p freemonplay-screen --lib --target "$target"
app="$PWD/target/ipad-$platform/Freemonplay Screen.app"
mkdir -p "$app"
xcrun --sdk "$sdk" clang -target "$triple" -isysroot "$(xcrun --sdk "$sdk" --show-sdk-path)" \
    build/ios/main.m "target/$target/debug/libfreemonplay_screen.a" \
    -framework UIKit -framework Foundation -framework CoreFoundation -framework CoreGraphics \
    -framework QuartzCore -framework Metal -framework CoreVideo -framework CoreMedia \
    -framework VideoToolbox -framework Security -framework SystemConfiguration \
    -framework AVFoundation -framework AudioToolbox -lresolv -liconv -lc++ \
    -o "$app/Freemonplay Screen"
python3 - "$app" "$platform" <<'PY'
import json, pathlib, plistlib, sys, tomllib
app = pathlib.Path(sys.argv[1])
version = tomllib.loads(pathlib.Path('Cargo.toml').read_text())['workspace']['package']['version']
info = dict(CFBundleExecutable='Freemonplay Screen', CFBundleIdentifier='io.github.moridin1138.freemonplay.screen',
    CFBundleName='Freemonplay Screen', CFBundleDisplayName='Freemonplay Screen', CFBundlePackageType='APPL',
    CFBundleShortVersionString=version, CFBundleVersion='1', MinimumOSVersion='16.0', UIDeviceFamily=[2],
    CFBundleSupportedPlatforms=['iPhoneSimulator' if sys.argv[2] == 'simulator' else 'iPhoneOS'],
    UISupportedInterfaceOrientations=['UIInterfaceOrientationPortrait', 'UIInterfaceOrientationPortraitUpsideDown', 'UIInterfaceOrientationLandscapeLeft', 'UIInterfaceOrientationLandscapeRight'],
    UILaunchScreen={}, UIRequiresFullScreen=True)
for locale in pathlib.Path('locales').glob('*.yml'):
    key = 'screen.plist.local_network_ipad:'
    value = next(json.loads(line[len(key):].strip()) for line in locale.read_text().splitlines() if line.startswith(key))
    folder = app / (locale.stem + '.lproj')
    folder.mkdir(exist_ok=True)
    (folder / 'InfoPlist.strings').write_text('"NSLocalNetworkUsageDescription" = ' + json.dumps(value, ensure_ascii=False) + ';\n')
    if locale.stem == 'en': info['NSLocalNetworkUsageDescription'] = value
(app / 'Info.plist').write_bytes(plistlib.dumps(info))
PY
if [ "$platform" = simulator ]; then
    codesign --force --sign - "$app"
fi
printf 'Built %s\n' "$app"
