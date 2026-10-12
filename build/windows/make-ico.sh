#!/usr/bin/env bash
# Builds build/windows/icon.ico (16 to 256 px) from the app icon, for the Windows client's
# .exe and installer. Run after changing build/macos/icon/icon-1024.png.
set -euo pipefail
cd "$(dirname "$0")/../.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sizes=(16 24 32 48 64 128 256)
for s in "${sizes[@]}"; do
    sips -z "$s" "$s" build/macos/icon/icon-1024.png --out "$tmp/$s.png" >/dev/null
done
# An .ico is a small directory followed by the images; modern Windows accepts PNG entries.
python3 - "$tmp" "${sizes[@]}" > build/windows/icon.ico <<'PY'
import struct, sys
d, sizes = sys.argv[1], [int(s) for s in sys.argv[2:]]
images = [open(f"{d}/{s}.png", "rb").read() for s in sizes]
out = struct.pack("<HHH", 0, 1, len(images))
offset = 6 + 16 * len(images)
for s, data in zip(sizes, images):
    out += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
sys.stdout.buffer.write(out + b"".join(images))
PY
echo "Built build/windows/icon.ico"
