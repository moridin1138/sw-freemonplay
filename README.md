# freemonplay

Turn any spare laptop, tablet or PC into an extra monitor for your Mac, over the network. Free and open source.

The **host** (the computer that wants more screens) creates a real virtual monitor for each **client** that connects. The OS treats it like a plugged-in display: drag windows onto it, arrange it in Display settings, use your normal mouse on it. The host captures that monitor, encodes it as H.264 and streams it to the client. The client shows it full screen and sends touch, mouse and keyboard back.

Virtual monitors don't use the GPU's display engines, so the "1 or 2 external displays" limit on base Apple Silicon Macs doesn't apply. Each client is one more screen.

## Status

| Part | macOS | Windows | Linux |
|---|---|---|---|
| Client (view + input) | ✅ | should work; not yet built or tested | should work; not yet built or tested |
| Host: virtual display | ✅ `CGVirtualDisplay` | planned: Virtual Display Driver (IDD) | planned: Mutter / wlroots / xrandr |
| Host: capture | ✅ ScreenCaptureKit | planned: Windows.Graphics.Capture | planned: PipeWire |
| Host: input injection | ✅ CGEvent | planned: SendInput | planned: libei / uinput |

Tested so far: virtual display creation (including HiDPI) on macOS 15 / M4, plus the full network → encode → decode → render → input path using the host's `--test-pattern` mode. The real capture path needs Screen Recording permission (see below), so it hasn't been run end to end yet.

## Build

Requires Rust (`brew install rustup && rustup default stable`) and, on macOS, the Xcode Command Line Tools. Full Xcode isn't needed.

```bash
cargo build --release
```

Binaries end up in `target/release/freemonplay-host` and `target/release/freemonplay-client`.

## Use

On the Mac that wants more screens:

```bash
./target/release/freemonplay-host
```

It prints a 6 digit pairing code. The first run asks for two permissions for your terminal app (System Settings > Privacy & Security):
- **Screen & System Audio Recording**, so it can capture the virtual displays. Restart the terminal after granting it.
- **Accessibility**, so clients can control the mouse and keyboard. Without it the screens are view-only.

On each device that should become a screen:

```bash
./freemonplay-client
```

It finds hosts on the LAN, asks for the pairing code the first time, then goes full screen. After pairing it reconnects without a code.

Useful options:

| | |
|---|---|
| `client --host 192.168.1.20` | Connect directly (skip discovery, e.g. across subnets) |
| `client --windowed 1280x800` | Run in a window instead of full screen |
| `client --monitor 1` | Fill a different monitor |
| `client --scale 2` | Force Retina-style 2x rendering (`auto` uses it on high-density screens) |
| `host --max-fps 30 --bitrate-mbps 8` | Lower bandwidth |
| `host --no-input` | View only; ignore client input |
| `host --forget-clients` | Unpair every client |
| `host --test-pattern` | Stream a generated picture instead of a virtual display (no permissions needed; any OS) |
| `host --probe-display 2732x2048@2x` | Create a virtual display for 3 s to check the OS supports it |

Client shortcuts (not sent to the host): **Ctrl+Alt+Shift+F** toggles full screen, **Ctrl+Alt+Shift+Q** quits.

Quick test on one Mac: run the host, then `freemonplay-client --host 127.0.0.1 --windowed 1280x800`. A new display appears in System Settings > Displays. Drag a window onto it and it shows up in the client window.

## How it works

```
crates/
  proto/    wire protocol shared by both sides
  host/     mDNS advert, QUIC server, per-client session
    platform/macos/   virtual_display.rs, capture.rs, input.rs
    encoder.rs        OpenH264 (software, same on every OS)
  client/   discovery, QUIC client, decoder thread, wgpu renderer, input mapping
```

- **Transport:** one QUIC connection per screen (`quinn`). Client to host is a control stream (hello, input, keyframe requests). Host to client is a video stream of H.264 access units. Messages are length-prefixed `postcard`.
- **Latency:** the host keeps only the newest captured frame. If encoding or the network falls behind, frames are skipped *before* encoding, so the picture never queues up. The client does the same after decoding.
- **Pairing:** the host has a self-signed certificate. A new client must send the host's pairing code; the host then issues a random token (it stores only its hash) and the client pins the host's certificate fingerprint. Five wrong codes rotate the code.
- **Display identity:** each client gets a stable serial number, so macOS remembers where you arranged that screen.

## Known limitations and next steps

- **Software H.264.** OpenH264 is portable but CPU-hungry above 1440p. Next: VideoToolbox on the host and hardware decode on clients.
- **Bitrate control is loose.** Frame skipping is off to avoid stutter, and OpenH264 can't cap bitrate strictly without it. That's fine on a LAN but could be tuned.
- **First-pairing trust.** Someone actively intercepting your LAN during the very first pairing could sit in the middle. After pairing, the pinned fingerprint prevents this.
- **No GUI yet.** Both sides are command line tools. Next: a tray app for the host and a host picker for the client.
- **Windows and Linux hosts** aren't implemented yet (see `crates/host/src/platform/unsupported.rs` for the plan).
- **Touch** maps to a single-finger mouse. Multi-touch gestures aren't forwarded.

## Licensing notes

The code is MIT or Apache-2.0. The OpenH264 library is compiled from source (BSD licensed). H.264 itself is patent-encumbered: Cisco's royalty coverage applies only to its own prebuilt binaries, not to builds from source. That's generally a non-issue for personal use, but check before distributing binaries commercially.
