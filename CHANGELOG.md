# Changelog

What changed in each release, newest first. Work in progress goes under **Unreleased**;
tagging a release renames that section to the version and date, and the release workflow
publishes it as the release notes.

## [Unreleased]

Hosts and screens must both be updated: the protocol changed (version 11).

### Changed
- **Shorter, clearer text** throughout both apps: menus, the setup windows, the screen
  app's window and its messages. A rejected pairing or a version mismatch now says what to
  do about it.
- **New names: Freemonplay Host and Freemonplay Screen.** The Mac apps are now
  "Freemonplay Host.app" (was "freemonplay.app") and "Freemonplay Screen.app", the Mac host
  download is `freemonplay-host-macos.dmg`, and the screen's command is `freemonplay-screen`
  (was `freemonplay-client`) on every OS. Installing a new build replaces the old copies.
  Pairings and settings carry over.
- **Disconnecting is easier:** move the pointer to the top edge of a screen for a toolbar
  with the host's name, the stream's size, codec and frame rate, and Full Screen and
  Disconnect buttons ("Show a toolbar at the top edge" in Options turns it off). The list
  then says which computer you disconnected from.
- **The screen app remembers its options** (full screen, sharpness, codec, keeping awake,
  clipboard, toolbar, waiting for the refresh) between launches. Command line flags still
  win for that run.
- **Logs say more:** the screen app now keeps a log file (Show log at the bottom of its
  window), the host logs why each session ended (closed by the screen, timed out, closed
  from the menu), and both start a fresh file once the log passes 5 MB.
- **A clearer client window:** the real app icon, an amber Connect button for each
  computer (with whether it's paired), the computer itself no longer listed, "Use this
  screen as: Extra screen / Keyboard and mouse only" in plain words, a larger pairing code
  field, shortcuts written with each OS's key names, and the version at the bottom. Windows
  and Linux windows show the app icon too.

### Added
- **Languages:** both apps now speak English, German, Spanish, French, Turkish and
  Simplified Chinese, following the computer's language. Translations live in the
  `locales` folder, one file per language; CONTRIBUTING.md explains how to add one.
- **Release signing, ready to switch on:** tagged releases sign the Windows programs and
  installers through the SignPath Foundation, and Developer ID sign and notarize the Mac
  apps and disk images, once the accounts and secrets in SIGNING.md exist. The README has
  the code signing policy.
- **Windows host: a tray app and an installer.** Freemonplay on Windows now lives in the
  notification area, with the same menu as on a Mac: the pairing code, connected screens
  with live stats and Disconnect, the codec, frame rate, input and clipboard settings,
  Start at Sign-in, Setup Guide and the log. A setup dialog checks the display driver and
  the firewall rule, installs either with one click (Windows asks for administrator rights)
  and shows the pairing code. The new `freemonplay-host-windows-x64-setup.exe` installs
  the host with the Virtual Display Driver and the firewall rule in one go.
- **Windows host: automatic display scaling.** Each screen's virtual monitor gets that
  screen's own scale (a Retina Mac 200%, a PC set to 150% gets 150%), so text appears the
  size it would there. Before, Windows picked 100% for a new 2940x1912 monitor, and
  everything was tiny. Screens now tell the host their scale (protocol 11).
- **Riding out dropped connections:** when a screen's connection drops out (Wi-Fi gone for
  more than a few seconds, a laptop roaming), the screen keeps the last picture and
  reconnects by itself for 15 s, with a Stop button. The host keeps that screen's display
  meanwhile, so the windows on it stay where they were instead of jumping to the main
  display. Disconnecting on purpose still removes it straight away.
- **A setup window on the Mac host:** on first launch (and whenever Screen Recording is
  missing) a window walks through Screen Recording, Accessibility and connecting a first
  screen, with live status for each permission, buttons that open the right System
  Settings page, a Restart button once Screen Recording is newly allowed, the pairing code
  with Copy Code, and a link to download Freemonplay Screen. "Setup Guide…" in the menu
  reopens it.
- **Windows host, preview:** a Windows PC can now get extra screens too. Each screen
  extends the desktop onto a virtual monitor at its own resolution, through the signed,
  MIT licensed Virtual Display Driver (install it once, as administrator, with
  `freemonplay-host --install-display-driver <folder>`; without it, screens mirror the
  primary monitor). Windows.Graphics.Capture (cursor included), scaled and converted to
  NV12 on the GPU, hardware encoded by Media Foundation (H.264 or HEVC, e.g. Intel Quick
  Sync) on the same device, so frames never pass through system memory; OpenH264 is the
  fallback. Input by `SendInput`, and the shared clipboard. Tested on a Surface (Iris Xe)
  extending onto a MacBook Air at 2940x1912 HEVC.
- **Mac downloads are disk images:** open one and drag the app to Applications.
- **Copied images are shared too** (screenshots, images copied from apps), both ways,
  as PNG up to 24 MB. They travel on streams of their own, so a big one doesn't hold up
  the keyboard and mouse. When something copied has both text and an image, the text is
  shared.
- Mac and Linux screens report their network link to the host's menu too (Wi-Fi band,
  signal and link rate, or wired, with tips), as Windows screens already did. Linux uses
  NetworkManager.

### Fixed
- The host now accepts IPv4 connections on Windows too (its listening socket is explicitly
  dual-stack; Windows makes IPv6 sockets IPv6-only by default).
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
