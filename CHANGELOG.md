# Changelog

What changed in each release, newest first. Work in progress goes under **Unreleased**;
tagging a release renames that section to the version and date, and the release workflow
publishes it as the release notes.

## [Unreleased]

### Added
- Mac and Linux screens report their network link to the host's menu too (Wi-Fi band,
  signal and link rate, or wired, with tips), as Windows screens already did. Linux uses
  NetworkManager.

### Fixed
- The burst of frames that arrives when Wi-Fi recovers from a stall no longer makes the
  client drop video and ask for a keyframe (the biggest kind of frame, on a link that had
  just stalled). It now decodes the burst, which takes milliseconds, and shows the newest
  picture. It only skips ahead when frames wait over 300 ms for a decoder that can't keep
  up.

### Changed
- CI uses the Node 24 versions of GitHub's actions.

## [0.2.0] - 2026-10-12

Hosts and clients must both be updated: the protocol changed (version 9), and 0.1.0
clients can't connect to a 0.2.0 host or the other way round.

### Added
- **Installers.** Windows screens get a setup program (per user, no administrator rights;
  a newer one updates in place) and Macs used as screens get a "freemonplay Screen" app.
  The Windows client has its own icon and no longer opens a console window when started
  from the Start menu. The Linux download includes a desktop entry and icon.
- **Shared clipboard.** Text copied on the Mac can be pasted on the screens' computers and
  the other way round, in ordinary screen sessions as well as keyboard and mouse mode.
  Passwords copied from password managers (marked concealed or transient on macOS, or
  excluded from clipboard monitoring on Windows) are never shared. "Share Clipboard with
  Screens" in the menu and "Share the clipboard with the host" on the client (or
  `--no-clipboard`) turn it off.
- **Keyboard and mouse mode** (a software KVM; Windows and Mac clients). Each of a
  client's monitors can be a *screen* for the Mac, *keyboard and mouse* (stays the
  client's own, but the Mac's keyboard and mouse can move onto it) or unused. Mixed
  example: a PC with three monitors lends one to the Mac as a screen and keeps two, which
  the Mac's pointer reaches by moving off that screen's edges, as on the desk. With no
  screen at all, the client's monitors sit beside the Mac's main display (side chosen in
  the menu). Ctrl+Alt+Shift+K on the Mac takes the
  keyboard and mouse back from anywhere. "Mac shortcuts" (on by default) makes Command
  act as Ctrl. `--keyboard-mouse` uses all the client's monitors this way. Macs used as
  screens can be controlled too (they need the Accessibility permission).
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
- **Delay and loss measurements:** each screen reports how many frames were delayed in
  transit (and by how much), and the host logs packet loss, so you can tell Wi-Fi
  stalls from a slow link.
- **Several monitors on one client.** Set other monitors to "Screen for the host" (or pass
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
- Adaptive bitrate no longer mistakes Wi-Fi stalls for a slow link. On a busy 2.4 GHz
  network it had cut the bitrate to under 4 Mbit/s while only about 3 Mbit/s was being
  sent, lowering quality with no gain. Stalls now count only when the link is carrying
  most of the bitrate, or when they last 2 s or more.
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

[Unreleased]: https://github.com/moridin1138/sw-freemonplay/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/moridin1138/sw-freemonplay/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/moridin1138/sw-freemonplay/releases/tag/v0.1.0
