<img src="build/macos/icon/icon-1024.png" alt="freemonplay icon" width="128" align="right">

# freemonplay

Turn any spare laptop, tablet or PC into an extra monitor for your Mac, over the network. Free and open source.

The **host** (the computer that wants more screens) creates a real virtual monitor for each **client** screen that connects. The OS treats it like a plugged-in display: drag windows onto it, arrange it in Display settings, use your normal mouse on it. The host captures that monitor, encodes it as H.264 and streams it to the client. The client shows it full screen and sends touch, mouse and keyboard back.

Virtual monitors don't use the GPU's display engines, so the "1 or 2 external displays" limit on base Apple Silicon Macs doesn't apply. Each client monitor is one more screen, and a client with several monitors can lend all of them.

See [CHANGELOG.md](CHANGELOG.md) for what's new, [ARCHITECTURE.md](ARCHITECTURE.md) for how it works and [BACKLOG.md](BACKLOG.md) for what's next.

## Status

| Part | macOS | Windows | Linux |
|---|---|---|---|
| Client (view + input) | ✅ | ✅ (tested on a Surface, Iris Xe) | builds; not yet run on Linux |
| Host: virtual display | ✅ `CGVirtualDisplay` | planned: Virtual Display Driver (IDD) | planned: Mutter / wlroots / xrandr |
| Host: capture | ✅ ScreenCaptureKit | planned: Windows.Graphics.Capture | planned: PipeWire |
| Host: input injection | ✅ CGEvent | planned: SendInput | planned: libei / uinput |
| Host: hardware encoding | ✅ VideoToolbox (H.264, HEVC) | planned: Media Foundation / NVENC | planned: VAAPI |
| Client: decoding (H.264, HEVC) | ✅ VideoToolbox (hardware) | ✅ Media Foundation, DXVA on the GPU (software fallback) | GStreamer: VAAPI hardware, else system software decoder |
| Client: zero-copy display | ✅ Metal | ✅ Direct3D 12 | planned |
| Client: several monitors | ✅ | ✅ (not yet tested with real extra monitors) | ✅ (untested) |

Tested end to end: a Mac host (macOS 15, M4) streaming to Mac and Windows clients over Wi-Fi. `freemonplay-client --self-test` checks a machine's decoder and GPU path without a window.

## Build

Requires Rust (`brew install rustup && rustup default stable`) and, on macOS, the Xcode Command Line Tools. Full Xcode isn't needed.

```bash
cargo build --release
```

Binaries end up in `target/release/freemonplay-host` and `target/release/freemonplay-client`.

**Linux client:** needs GStreamer's development files to build and its H.264 plugins to run. On Debian/Ubuntu:

```bash
sudo apt install libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-bad gstreamer1.0-libav intel-media-va-driver
```

Or build it from a Mac (or any machine with Docker), without a Linux box:

```bash
./build/linux/build.sh
```

That builds an x86_64 binary in an Ubuntu 22.04 container, at `target/linux-x86_64/release/freemonplay-client`. It runs on Ubuntu 22.04+, Debian 12+ and other distros from 2022 onwards. The target machine still needs GStreamer's runtime plugins: `sudo apt install gstreamer1.0-plugins-bad gstreamer1.0-libav intel-media-va-driver`.

`intel-media-va-driver` gives Intel GPUs (including low-power ones like Jasper Lake) hardware decoding; use `mesa-va-drivers` for AMD. Without GStreamer's H.264 plugins the client falls back to its bundled OpenH264. To force a particular GStreamer decoder, set `FMP_GST_DECODER` (for example `FMP_GST_DECODER=avdec_h264`).

**Windows client:** build natively with `cargo build --release -p freemonplay-client`, or cross-compile from a Mac:

```bash
brew install mingw-w64 && rustup target add x86_64-pc-windows-gnu
cargo build --release -p freemonplay-client --target x86_64-pc-windows-gnu
```

The `.exe` (in `target/x86_64-pc-windows-gnu/release/`) is self-contained and only uses DLLs that ship with Windows 10/11. Windows "N" editions need Microsoft's Media Feature Pack for video decoding.

## Use

On the Mac that wants more screens, install the menu bar app:

```bash
./build/macos/bundle.sh --install
```

Then open **freemonplay** from Launchpad or Spotlight. It lives in the menu bar (no Dock icon) and shows the pairing code, connected screens with live stats (frame rate, bitrate and estimated latency, with a breakdown per screen) and a Disconnect option for each, plus the codec, frame rate and input settings, Open at Login and the log. Install it to /Applications as above: macOS can stall apps that run from inside Documents, Desktop or Downloads behind file-access prompts.

On MacBooks with a notch, menu bar icons that don't fit are hidden behind it. If you can't see the freemonplay icon, open freemonplay again (from Spotlight or Launchpad) and its menu appears at the pointer, or Cmd-drag other icons to make room.

**Keep permissions across rebuilds (once per Mac):** macOS ties Screen Recording and Accessibility permissions to the app's signature. By default the app gets a throwaway (ad-hoc) signature, so every rebuild needs the permissions re-enabled. Create a local signing identity once and `bundle.sh` uses it automatically:

```bash
./build/macos/make-signing-identity.sh
```

It adds a self-signed "freemonplay Local Signing" certificate to your login keychain and asks for your password to trust it for code signing. It only affects apps you build on this Mac. Grant the permissions one more time after the first signed build; after that they stick.

You can also run it in a terminal, which prints the pairing code:

```bash
./target/release/freemonplay-host
```

The first run asks for two permissions (System Settings > Privacy & Security), for freemonplay when you use the app, or for your terminal app when you run it in a terminal:
- **Screen & System Audio Recording**, so it can capture the virtual displays. Restart the app or terminal after granting it.
- **Accessibility**, so clients can control the mouse and keyboard. Without it the screens are view-only.

On each device that should become a screen:

```bash
./freemonplay-client
```

A window lists the computers running freemonplay on your network. Drag it to the monitor you want to use, click **Connect**, and enter the pairing code shown in the host's menu the first time (after that it reconnects without one). The screen fills that monitor; disconnecting brings the list back. If the device has more monitors, tick them under **Also use these monitors** and each becomes another screen on the host (arrange them in the host's Display settings; they're remembered). Hosts on other networks can be reached with **Connect by address**, and **Options** sets full screen, sharpness (1x/2x), codec, vsync and whether to keep the screen awake while connected (on by default).

Useful options:

| | |
|---|---|
| `client --host 192.168.1.20` | Connect straight away instead of showing the list (add `--code 123456` the first time) |
| `client --windowed 1280x800` | Stream in a window instead of full screen |
| `client --monitor 1` | Open the window on a different monitor |
| `client --all-monitors` | Use all of this device's monitors, each as its own screen |
| `client --self-test` | Check this machine's video decoder and GPU display path, then exit (no window needed) |
| `client --scale 2` | Force Retina-style 2x rendering (`auto` uses it on high-density screens) |
| `client --allow-sleep` | Let the screen dim, lock or sleep while connected (by default it's kept awake) |
| `client --no-vsync` | Show frames without waiting for the display refresh: up to one refresh (about 16 ms at 60 Hz) less latency, but can tear |
| `host --max-fps 30 --bitrate-mbps 8` | Lower bandwidth |
| `host --encoder software` | Force the OpenH264 software encoder (default `auto` prefers hardware) |
| `host --codec h264` / `client --codec h264` | Limit to one codec (default `auto` negotiates; see below) |
| `host --no-input` | View only; ignore client input |
| `host --forget-clients` | Unpair every client |
| `host --test-pattern` | Stream a generated picture instead of a virtual display (no permissions needed; any OS) |
| `host --probe-display 2732x2048@2x` | Create a virtual display for 3 s to check the OS supports it |

Client shortcuts while connected (not sent to the host): **Ctrl+Alt+Shift+D** disconnects all screens (back to the list), **Ctrl+Alt+Shift+F** toggles full screen, **Ctrl+Alt+Shift+Q** quits.

Quick test on one Mac: run the host, then `freemonplay-client --host 127.0.0.1 --windowed 1280x800`. A new display appears in System Settings > Displays. Drag a window onto it and it shows up in the client window.

## How it works

One QUIC connection per screen: the host captures its virtual display, hardware encodes it (H.264 or HEVC, negotiated) and streams it; the client decodes with the OS's decoder, draws it with wgpu and sends input back. Pairing uses SPAKE2 with the host's 6 digit code, after which the client reconnects with a token. Frames are skipped rather than queued whenever anything falls behind, so the picture never lags. Details, including threads, zero copy, several monitors and the protocol, are in [ARCHITECTURE.md](ARCHITECTURE.md).

## Known limitations and next steps

- **Hardware encoding is macOS only.** On a Mac, captured frames go straight from ScreenCaptureKit into the hardware encoder with no copy (VideoToolbox's low-latency mode, Constrained High profile). Other hosts would fall back to OpenH264, which is CPU-hungry above 1440p.
- **Clients decode with the OS's decoder**: VideoToolbox on macOS, Media Foundation with DXVA on Windows, GStreamer (VAAPI where available) on Linux.
- **Mac and Windows clients draw the decoder's output directly** (zero copy). On a Mac at 1080p60 that took client CPU from 11.7% to 3.6% and received-to-shown time from 27 ms to under 2 ms; on a Surface it cut decoding from 4.9 to 1.8 ms per frame. Set `FMP_ZERO_COPY=0` to fall back to copying if a GPU ever misbehaves. Linux clients still copy decoded pictures through system memory.
- **Wi-Fi adds most of the latency.** A typical figure is about 50 ms in total, of which the network is roughly two thirds. Wired Ethernet, 5 GHz Wi-Fi and turning off the client's Wi-Fi power saving all help.
- **HEVC on Windows clients** needs Microsoft's "HEVC Video Extensions" (often preinstalled by PC makers). Without them, Windows clients use H.264.
- **HEVC on Linux clients** is offered automatically only with a VAAPI HEVC decoder (`vah265dec`/`vaapih265dec`); `--codec h265` forces it in software via `avdec_h265`.
- **Bitrate control with the software encoder is loose.** Frame skipping is off to avoid stutter, and OpenH264 can't cap bitrate strictly without it. That's fine on a LAN.
- **Client app packaging.** The client is a single program with its own window, but not yet a Mac `.app`, Windows installer or Linux desktop entry.
- **Windows and Linux hosts** aren't implemented yet (see `crates/host/src/platform/unsupported.rs` for the plan).
- **Touch** maps to a single-finger mouse. Multi-touch gestures aren't forwarded.

## License

Copyright (C) 2026 moridin1138

freemonplay is free software: you can redistribute it and/or modify it under the terms of the GNU General Public License as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See [LICENSE](LICENSE) for details.

In short: anyone can use, study, change and share it, but anything distributed that includes it must also be GPL, with source.

All dependencies are under GPL-compatible licenses (MIT, Apache-2.0, BSD, ISC, Zlib and one MPL-2.0 crate). On Linux the client uses the system's GStreamer (LGPL) through dynamic linking.

### H.264 and HEVC patents

These codecs are patent-encumbered, separately from copyright:
- **Hardware paths:** encoding on macOS and decoding on macOS and Windows use the operating system's built-in codecs, which ship licensed with the OS. On Linux the codecs come from the distribution's GStreamer plugins.
- **OpenH264** is compiled from source (BSD licensed) only for the host's software fallback encoder and the Linux client's last-resort decoder. Cisco's royalty coverage applies only to its own prebuilt binaries, not to builds from source.

That's generally a non-issue for personal use, but check before distributing binaries commercially.
