# iPad screen preview

Requires full Xcode, Rust and Python 3.11+. The app uses iPadOS 16 or newer, in portrait and landscape.
It shares the desktop screen client's pairing, network, decoder and renderer. Connect by
IP address and enter the host's pairing code. Mac and Windows hosts use the same protocol.

If Xcode is outside /Applications, set `DEVELOPER_DIR` to its Contents/Developer folder.
This only affects the current shell, for example:

```bash
export DEVELOPER_DIR="$HOME/Downloads/Xcode.app/Contents/Developer"
./build/ios/build.sh simulator
```

Boot an iPad simulator in Xcode, then install and launch:

```bash
xcrun simctl install booted 'target/ipad-simulator/Freemonplay Screen.app'
xcrun simctl launch booted io.github.moridin1138.freemonplay.screen
```

`./build/ios/build.sh device` produces an unsigned ARM64 device app in
`target/ipad-device/Freemonplay Screen.app`. For distribution signing and TestFlight, use the Xcode archive workflow below. Simulator builds do not prove hardware decoding or local network
permission works on an iPad.

The preview has no automatic discovery, clipboard sharing, keyboard and mouse mode or
external monitor support. The Disconnect bar stays visible for touch. Leaving the app
ends the session. Rotation fits the existing host picture to the new window shape, with black bars as needed.
The host display keeps the resolution selected when connecting. Use an isolated test host
as described in AGENTS.md when testing locally.

## Xcode and TestFlight

Open `build/ios/Freemonplay.xcodeproj` and select the **Freemonplay Screen** scheme.
The project uses the maintainer's Apple team and automatic signing. Rust, its iOS targets
and Python 3.11+ must be installed. The build phase compiles the shared Rust library,
then Xcode builds the app, icon, privacy manifest and localized network prompts.

To archive and export an App Store signed package from a shell:

```bash
export DEVELOPER_DIR="$HOME/Downloads/Xcode.app/Contents/Developer"
FMP_BUILD_NUMBER=1 ./build/ios/archive.sh
```

Increase `FMP_BUILD_NUMBER` for each upload of the same version. `FMP_APPLE_TEAM` overrides
the signing team. In Xcode, the equivalent fields are Current Project Version and Team.
Keep the project's Marketing Version in step with the workspace version in Cargo.toml.

The archive is `target/Freemonplay-TestFlight.xcarchive`; the signed IPA is in
`target/ipad-testflight`. The script exports locally. To upload, open the archive in
Xcode Organizer and choose **Distribute App > App Store Connect**. Create the app record
with bundle ID `io.github.moridin1138.freemonplay.screen` first.

Apple must process the uploaded build before it can be added to an internal TestFlight
group. Complete its encryption questions in App Store Connect: the client includes
rustls/ring TLS and SPAKE2 pairing, rather than relying only on Apple's system TLS.
External testers need beta review. Signing keys and provisioning profiles stay outside
the repository; generated archives and exports are under the ignored target directory.
