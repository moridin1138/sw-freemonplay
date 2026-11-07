#!/bin/bash
set -euo pipefail
cd "$SRCROOT/../.."
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
export IPHONEOS_DEPLOYMENT_TARGET=16.0
case "$PLATFORM_NAME" in
    iphoneos) rust_target=aarch64-apple-ios ;;
    iphonesimulator) rust_target=aarch64-apple-ios-sim ;;
    *) exit 1 ;;
esac
rustup target add "$rust_target"
profile=debug
args=()
if [ "$CONFIGURATION" = Release ]; then profile=release; args+=(--release); fi
cargo build -p freemonplay-screen --lib --target "$rust_target" "${args[@]}"
cp "target/$rust_target/$profile/libfreemonplay_screen.a" "$BUILT_PRODUCTS_DIR/libfreemonplay_screen.a"
# Localize the system network prompt from the same files as both desktop apps.
python3 - "$BUILT_PRODUCTS_DIR/$UNLOCALIZED_RESOURCES_FOLDER_PATH" <<'PY'
import json
from pathlib import Path
import sys
app = Path(sys.argv[1])
for locale in Path('locales').glob('*.yml'):
    key = 'screen.plist.local_network_ipad:'
    value = next(json.loads(line[len(key):].strip()) for line in locale.read_text().splitlines() if line.startswith(key))
    folder = app / (locale.stem + '.lproj')
    folder.mkdir(parents=True, exist_ok=True)
    (folder / 'InfoPlist.strings').write_text('"NSLocalNetworkUsageDescription" = ' + json.dumps(value, ensure_ascii=False) + ';\n')
PY
