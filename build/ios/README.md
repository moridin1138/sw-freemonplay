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
`target/ipad-device/Freemonplay Screen.app`. Device provisioning, signing and installation
are not provided yet. Simulator builds do not prove hardware decoding or local network
permission works on an iPad.

The preview has no automatic discovery, clipboard sharing, keyboard and mouse mode or
external monitor support. The Disconnect bar stays visible for touch. Leaving the app
ends the session. Rotation fits the existing host picture to the new window shape, with black bars as needed.
The host display keeps the resolution selected when connecting. Use an isolated test host
as described in AGENTS.md when testing locally.
