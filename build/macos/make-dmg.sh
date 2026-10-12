#!/usr/bin/env bash
# Packs an app into a disk image that opens to the app beside an Applications shortcut,
# so installing is a drag across.
#   ./build/macos/make-dmg.sh "target/Freemonplay Host.app" dist/freemonplay-host-macos.dmg
set -euo pipefail

app=$1
out=$2
name=$(basename "$app" .app)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

# ditto keeps the code signature and extended attributes intact.
ditto "$app" "$stage/$name.app"
ln -s /Applications "$stage/Applications"
mkdir -p "$(dirname "$out")"
hdiutil create -volname "$name" -srcfolder "$stage" -fs HFS+ -format UDZO -imagekey zlib-level=9 -ov "$out" >/dev/null
echo "Built $out"
