#!/usr/bin/env bash
# Renders the app icon (every size macOS wants, packed into AppIcon.icns) and the menu bar
# template image (menubar.pdf, embedded in the host binary).
# Run after editing make-icon.swift; the results are committed so normal builds don't
# need Swift.
set -euo pipefail

cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

swift make-icon.swift icon-1024.png
iconset="$tmp/AppIcon.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" icon-1024.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    sips -z $((size * 2)) $((size * 2)) icon-1024.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o AppIcon.icns
swift make-menubar-icon.swift menubar.pdf >/dev/null
echo "Wrote build/macos/icon/AppIcon.icns, icon-1024.png and menubar.pdf"
