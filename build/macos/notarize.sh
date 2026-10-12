#!/usr/bin/env bash
# Notarizes a Developer ID signed app (bundle.sh with FMP_SIGN_IDENTITY set), staples it,
# and packs it into a signed, notarized and stapled disk image.
#   ./build/macos/notarize.sh "target/Freemonplay Host.app" dist/freemonplay-host-macos.dmg
# Needs FMP_SIGN_IDENTITY ("Developer ID Application: Name (TEAMID)") and an App Store
# Connect API key: ASC_KEY_PATH (the .p8 file), ASC_KEY_ID and ASC_ISSUER_ID.
set -euo pipefail

app=$1
dmg=$2
notary=(--key "$ASC_KEY_PATH" --key-id "$ASC_KEY_ID" --issuer "$ASC_ISSUER_ID" --wait)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

codesign --verify --strict --verbose=2 "$app"
# The app first (a zip can't be stapled, the app can), so it also passes Gatekeeper on its
# own after being copied out of the disk image offline.
ditto -c -k --keepParent "$app" "$work/app.zip"
xcrun notarytool submit "$work/app.zip" "${notary[@]}"
xcrun stapler staple "$app"

./build/macos/make-dmg.sh "$app" "$dmg"
codesign --force --timestamp --sign "$FMP_SIGN_IDENTITY" "$dmg"
xcrun notarytool submit "$dmg" "${notary[@]}"
xcrun stapler staple "$dmg"
spctl --assess --type open --context context:primary-signature --verbose "$dmg"
echo "Notarized $dmg"
