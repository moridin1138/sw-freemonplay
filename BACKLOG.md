# Backlog

Roughly in priority order. Done items move to [CHANGELOG.md](CHANGELOG.md).

## In progress

- **Keyboard and mouse mode.** Built (protocol 8) for Windows and Mac clients: mixed and keyboard-and-mouse-only layouts, text clipboard, escape shortcut. Keyboard-and-mouse-only layout tested on the Surface: crossing, return by edge and release on disconnect all work. The mixed layout still needs a client with a second monitor. Shared clipboard tested both ways with the Surface (0.2.0), including a concealed (password manager) item staying on the Mac; the Windows private-item marker is untested. Images tested both ways too (protocol 10), including a 2736x1824 one in about a second. Mac clients built too (CGEvent; untested, needs a second Mac). Still to do: Linux clients (libei or uinput); files on the clipboard; a per-client layout editor if "side of the main display" turns out too coarse.
- **Several monitors on one client.** Built (protocol 5): each extra monitor is its own session and its own screen on the host. Needs testing with real extra monitors on Windows, and on Mac and Linux clients.
- **Keep client screens awake.** Tested on macOS. Windows (`SetThreadExecutionState`) and Linux (D-Bus ScreenSaver inhibit, with GNOME session manager fallback) still need checking on real machines.

- **Windows host.** Extends the desktop onto virtual monitors from the Virtual Display Driver (signed by SignPath; installed with `--install-display-driver`), hardware encoded with zero copy; tested Surface to MacBook Air. Learned the hard way: that driver can't be reconfigured while running (its reload command blocks its only control thread when monitors exist), so the installer fixes 3 monitors and a resolution list, and Windows rejects monitors offering too many modes (keep one global refresh rate). Next:
  - **Test the tray app and installer on a clean PC** (built and run on the Surface; the installer is only built by CI).
  - **Finer scaling:** the client only says whether it's HiDPI, so screens get 100% or 200%. Sending the client's own scale (e.g. 150%) would match it exactly (a protocol change).
  - The setup dialog only recognises its own firewall rule, not one made by accepting Windows' "allow access" prompt.
  - **Our own SudoVDA-based driver**, signed via SignPath, for true per-client monitors with stable identities (no fixed count or resolution list).
  - Keyboard and mouse mode with Windows as the host (low-level input hooks).

## Next

- **Wi-Fi, part 2.** Done: adaptive bitrate, capped send queue, Windows media streaming mode, link diagnostics on both ends. Still to do:
  - **Measured (Mac on 2.4 GHz, Surface on 5 GHz, 2026-10-12):** 0.0% packet loss, but 10 to 45 frames per 2 s delayed by 65 to 500 ms: Wi-Fi stalls, not loss. So forward error correction and reference frame invalidation (planned earlier; VideoToolbox does accept `EnableLTR` on an M4) wouldn't help on this network; revisit only if a network shows real loss. With both on 5 GHz (after moving the Surface off an access point that isolated its clients), delayed frames all but disappeared. Still worth testing: AWDL off on the Mac.
  - Check how often screens still ask for keyframes in normal use (the host now logs each request; one per connection is expected).
  - Check the adaptive bitrate's thresholds on a busy network (the 2.4 GHz Mac is a good test case).
- **Zero copy on Linux clients** (VAAPI surfaces as DMA-BUFs into wgpu's Vulkan backend).

## Later

- **Linux host**: GNOME/KDE/wlroots virtual monitors, PipeWire capture, VAAPI encode.
- **Android client**: Android tablets as extra screens (MediaCodec decoding).
- **Linux packaging**: AppImage or Flatpak (bundling GStreamer). The Mac client app and Windows installer are done.

## Distribution

- **Sign and notarize the Mac apps** (Apple Developer Program, about US$99 a year): no Gatekeeper warning, and permissions survive updates for everyone. Sign with the hardened runtime, notarize with `notarytool`, staple, all in release.yml with the certificate and an App Store Connect API key as secrets. The disk images get signed too.
- **Sign the Windows installer and client** to remove SmartScreen's warning: SignPath Foundation (free for open source), or Microsoft's Azure signing service (about US$10 a month).
- A disk image background with an arrow, if wanted (needs Finder layout, awkward on CI).
