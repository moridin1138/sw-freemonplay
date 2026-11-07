# Backlog

Roughly in priority order. Done items move to [CHANGELOG.md](CHANGELOG.md).

## In progress

- **Unified desktop app.** Check role changes and login items on Mac and Windows, plus
  installer upgrades from the old separate Windows packages on a clean PC.
- **iPad input latency.** Measure keyboard lag with a wired keyboard and a stable network
  to separate device input delay from host and network delay.

- **iPad screen.** Shared client builds as an iPadOS 16+ app with connection by address,
  pairing, VideoToolbox and Metal, touch input and a persistent Disconnect bar. Next:
  install through TestFlight and test streaming and touch against Mac and Windows hosts,
  native Bonjour discovery and a user initiated clipboard flow.

- **Keyboard and mouse mode.** Built (protocol 8) for Windows and Mac clients: mixed and keyboard-and-mouse-only layouts, text clipboard, escape shortcut. Keyboard-and-mouse-only layout tested on the Surface: crossing, return by edge and release on disconnect all work. The mixed layout still needs a client with a second monitor. Shared clipboard tested both ways with the Surface (0.2.0), including a concealed (password manager) item staying on the Mac; the Windows private-item marker is untested. Images tested both ways too (protocol 10), including a 2736x1824 one in about a second. Mac clients built too (CGEvent; untested, needs a second Mac). Still to do: Linux clients (libei or uinput); files on the clipboard; a per-client layout editor if "side of the main display" turns out too coarse.
- **Several monitors on one client.** Built (protocol 5): each extra monitor is its own session and its own screen on the host. Needs testing with real extra monitors on Windows, and on Mac and Linux clients.
- **Keep client screens awake.** Tested on macOS. Windows (`SetThreadExecutionState`) and Linux (D-Bus ScreenSaver inhibit, with GNOME session manager fallback) still need checking on real machines.

- **Windows host.** Extends the desktop onto virtual monitors from the Virtual Display Driver (signed by SignPath; installed with `--install-display-driver`), hardware encoded with zero copy; tested Surface to MacBook Air. Learned the hard way: that driver can't be reconfigured while running (its reload command blocks its only control thread when monitors exist), so the installer fixes 3 monitors and a resolution list, and Windows rejects monitors offering too many modes (keep one global refresh rate). Next:
  - **Test the installer on a clean PC.** The CI-built installer was tested on the Surface (2026-10-12): silent install as administrator put the driver, firewall rule and sign-in entry in place, the tray app started from Program Files, and uninstalling quit it and removed all of them. Still to try on a PC that never had the driver, with the normal (non-silent) installer.
  - **Our own SudoVDA-based driver** for true per-client monitors (needs driver signing, which Microsoft only grants through its hardware program with an EV certificate, so it depends on signing being sorted out first) with stable identities (no fixed count or resolution list).
  - **Test keyboard and mouse mode with Windows as the host** (built: low-level hooks, cursor parked and hidden while a screen has control). Both layouts, the escape shortcut, and a key held while crossing. Low-level hooks don't see input meant for programs running as administrator.

## Next

- **Wi-Fi, part 2.** Done: adaptive bitrate, capped send queue, Windows media streaming mode, link diagnostics on both ends. Still to do:
  - **Measured (Mac on 2.4 GHz, Surface on 5 GHz, 2026-10-12):** 0.0% packet loss, but 10 to 45 frames per 2 s delayed by 65 to 500 ms: Wi-Fi stalls, not loss. So forward error correction and reference frame invalidation (planned earlier; VideoToolbox does accept `EnableLTR` on an M4) wouldn't help on this network; revisit only if a network shows real loss. With both on 5 GHz (after moving the Surface off an access point that isolated its clients), delayed frames all but disappeared. Still worth testing: AWDL off on the Mac.
  - Check how often screens still ask for keyframes in normal use (the host now logs each request; one per connection is expected).
  - Check the adaptive bitrate's thresholds on a busy network (the 2.4 GHz Mac is a good test case).
- **Native speakers to review the translations** (de, es, fr, tr, zh-CN), which were written without one.
- **Zero copy on Linux clients** (VAAPI surfaces as DMA-BUFs into wgpu's Vulkan backend).

## Later

- **Linux host**: GNOME/KDE/wlroots virtual monitors, PipeWire capture, VAAPI encode.
- **Android client**: Android tablets as extra screens (MediaCodec decoding).
- **Linux packaging**: AppImage or Flatpak (bundling GStreamer). The Mac client app and Windows installer are done.

## Distribution

- **Desktop release signing, partly on hold** (SignPath declined, October 2026; see SIGNING.md). Free options worth doing first: sign Mac releases with one self-made certificate kept in CI, so Screen Recording and Accessibility survive updates (Gatekeeper still warns); and publish the Windows screen app through the Microsoft Store (free registration, free signing of MSIX packages; the host can't go there, it installs a driver). Apple Developer Program membership is active and the iPad can be signed for TestFlight. Desktop release signing is still to configure. Paid, when it's worth it: Microsoft Artifact Signing (about US$10 a month, open to individuals in Canada). Reapplying to SignPath after more releases is also possible.
- **Sign the Windows uninstallers** (Inno Setup's `SignedUninstaller` plus one more signing step).
- A disk image background with an arrow, if wanted (needs Finder layout, awkward on CI).
