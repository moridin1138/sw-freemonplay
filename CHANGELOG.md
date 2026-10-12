# Changelog

What changed in each release, newest first. Work in progress goes under **Unreleased**;
tagging a release renames that section to the version and date, and the release workflow
publishes it as the release notes.

## [Unreleased]

Hosts and clients must both be updated: the protocol changed (version 6).

### Added
- **Adaptive bitrate.** The host lowers the bitrate when the network can't keep up (sends
  stalling, or packet loss) and raises it again once things are calm, instead of letting
  video queue up. The menu shows the current bitrate and packet loss per screen.
- **Less queueing on slow links.** The host caps how much video may wait to be sent at
  about one round trip's worth, so a slow link skips frames instead of falling behind.
- **Wi-Fi tuning on Windows clients:** "media streaming mode" is switched on while
  connected, so the adapter doesn't doze between access point beacons.
- **Network diagnostics in the menu:** each screen's link (Wi-Fi standard, band, signal,
  link rate, or wired; Windows clients for now) and this Mac's own Wi-Fi, with tips such
  as "on 2.4 GHz" or "AWDL is active". `--self-test` reports the client's link too.
- **Several monitors on one client.** Tick "Also use these monitors" (or pass
  `--all-monitors`) and each of the client's monitors becomes its own screen on the host,
  remembered separately by macOS. The main window's screen keeps its identity, so existing
  arrangements stay put.
- **Zero copy on Windows clients.** Decoded frames stay on the GPU (shared Direct3D
  textures drawn by wgpu's Direct3D 12 backend). On a Surface (Iris Xe): 1.8 ms per frame
  versus 4.9 ms copying, and visibly smoother.
- **Keep the screen awake while connected** (on by default; `--allow-sleep` or the Options
  checkbox turns it off).
- **`--self-test`** on the client: decodes and draws a built-in clip with this machine's
  decoder and GPU, without a window, and reports which path ran.

### Changed
- When a host doesn't answer at all, the client explains the likely cause: a host it found
  on the network but can't reach points to Wi-Fi client isolation, guest networks or
  isolation between the router's bands, instead of just "no answer".

### Fixed
- Adaptive bitrate no longer drops at the start of every session (the first keyframe and
  screen paint are now ignored for 2 s).
- Windows client crashed on exit (or when closing the window) once zero copy was active.
- Windows client never reported latency with zero copy on, so the host's menu kept saying
  "waiting for the screen's first report".
- Mac client could write copied frames into the decoder's buffers after a zero-copy
  failure.

## [0.1.0] - 2026-10-11

First release.

### Added
- Mac host as a menu bar app: virtual displays (`CGVirtualDisplay`, HiDPI), ScreenCaptureKit
  capture, VideoToolbox hardware H.264 and HEVC encoding with zero-copy capture, input
  injection, live per-screen stats with a latency breakdown, Open at Login.
- Clients for macOS, Windows and Linux with a GUI (host list, pairing, options), using the
  OS's hardware decoders (VideoToolbox, Media Foundation, GStreamer/VAAPI) and an OpenH264
  fallback; zero-copy display on Mac clients.
- Codec negotiation, preferring HEVC where the client decodes it in hardware.
- Pairing with a 6 digit code (SPAKE2, bound to the TLS connection); later connections use
  a token and a pinned host certificate.
- mDNS discovery, connect by address, draw-on-arrival, optional `--no-vsync`.
- App icon and menu bar icon; local signing identity so permissions survive rebuilds.
- GPL-3.0-or-later license, CI for all three OSes, release builds for `v*` tags.

[Unreleased]: https://github.com/moridin1138/sw-freemonplay/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/moridin1138/sw-freemonplay/releases/tag/v0.1.0
