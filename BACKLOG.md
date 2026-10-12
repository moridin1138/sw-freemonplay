# Backlog

Roughly in priority order. Done items move to [CHANGELOG.md](CHANGELOG.md).

## In progress

- **Keyboard and mouse mode.** Built (protocol 8) for Windows clients: mixed and keyboard-and-mouse-only layouts, text clipboard, escape shortcut. Needs testing on the Surface. Still to do: macOS clients (CGEvent) and Linux clients (libei or uinput); images and files on the clipboard; clipboard sync for ordinary screen sessions too; a per-client layout editor if "side of the main display" turns out too coarse.
- **Several monitors on one client.** Built (protocol 5): each extra monitor is its own session and its own screen on the host. Needs testing with real extra monitors on Windows, and on Mac and Linux clients.
- **Keep client screens awake.** Tested on macOS. Windows (`SetThreadExecutionState`) and Linux (D-Bus ScreenSaver inhibit, with GNOME session manager fallback) still need checking on real machines.

## Next

- **Wi-Fi, part 2.** Done: adaptive bitrate, capped send queue, Windows media streaming mode, link diagnostics on both ends. Still to do:
  - **Measured (Mac on 2.4 GHz, Surface on 5 GHz, 2026-10-12):** 0.0% packet loss, but 10 to 45 frames per 2 s delayed by 65 to 500 ms: Wi-Fi stalls, not loss. So forward error correction and reference frame invalidation (planned earlier; VideoToolbox does accept `EnableLTR` on an M4) wouldn't help on this network; revisit only if a network shows real loss. Worth testing: the same session with AWDL off on the Mac, and with both on 5 GHz once that network lets them see each other.
  - Smoothing for stalls: after a stall, show the newest frame at once rather than catching up (check the client never displays a backlog), and consider a lower frame rate when the picture is mostly static.
  - Link diagnostics on Mac clients (CoreWLAN) and Linux clients (NetworkManager or `/proc/net/wireless`).
  - Check the adaptive bitrate's thresholds on a busy network (the 2.4 GHz Mac is a good test case).
- **Zero copy on Linux clients** (VAAPI surfaces as DMA-BUFs into wgpu's Vulkan backend).

## Later

- **Windows host** (extra screens for a Windows PC): bundle an Indirect Display Driver, capture with Windows.Graphics.Capture, hardware encode with Media Foundation/NVENC.
- **Linux host**: GNOME/KDE/wlroots virtual monitors, PipeWire capture, VAAPI encode.
- **Android client**: Android tablets as extra screens (MediaCodec decoding).
- **Client packaging**: Mac client `.app`, Windows installer, Linux desktop entry and AppImage/Flatpak.

## Housekeeping

- Bump `actions/checkout` and `actions/upload-artifact`/`download-artifact` to their Node 24 versions (GitHub warns that Node 20 actions are deprecated).
