#!/bin/bash
# Archive and sign locally. Upload remains an explicit Xcode Organizer action.
set -euo pipefail
cd "$(dirname "$0")/../.."
archive="$PWD/target/Freemonplay-TestFlight.xcarchive"
export_dir="$PWD/target/ipad-testflight"
team="${FMP_APPLE_TEAM:-2SUCQRS5Y6}"
xcodebuild -project build/ios/Freemonplay.xcodeproj -scheme 'Freemonplay Screen' \
    -destination 'generic/platform=iOS' -archivePath "$archive" archive \
    DEVELOPMENT_TEAM="$team" CURRENT_PROJECT_VERSION="${FMP_BUILD_NUMBER:-1}" CODE_SIGNING_ALLOWED=NO
python3 - "$team" <<'PY'
import plistlib, sys
with open('target/ipad-export-options.plist', 'wb') as f:
    plistlib.dump(dict(method='app-store-connect', destination='export', signingStyle='automatic',
                       teamID=sys.argv[1], manageAppVersionAndBuildNumber=False), f)
PY
xcodebuild -exportArchive -archivePath "$archive" -exportPath "$export_dir" \
    -exportOptionsPlist target/ipad-export-options.plist -allowProvisioningUpdates
