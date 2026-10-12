# Backlog

Roughly in priority order. Done items move to [CHANGELOG.md](CHANGELOG.md).

## In progress

- **Keyboard and mouse mode.** Built (protocol 8) for Windows and Mac clients: mixed and keyboard-and-mouse-only layouts, text clipboard, escape shortcut. Keyboard-and-mouse-only layout tested on the Surface: crossing, return by edge and release on disconnect all work. The mixed layout still needs a client with a second monitor. Shared clipboard tested both ways with the Surface (0.2.0), including a concealed (password manager) item staying on the Mac; the Windows private-item marker is untested. Images tested both ways too (protocol 10), including a 2736x1824 one in about a second. Mac clients built too (CGEvent; untested, needs a second Mac). Still to do: Linux clients (libei or uinput); files on the clipboard; a per-client layout editor if "side of the main display" turns out too coarse.
- **Several monitors on one client.** Built (protocol 5): each extra monitor is its own session and its own screen on the host. Needs testing with real extra monitors on Windows, and on Mac and Linux clients.
- **Keep client screens awake.** Tested on macOS. Windows (`SetThreadExecutionState`) and Linux (D-Bus ScreenSaver inhibit, with GNOME session manager fallback) still need checking on real machines.

- **Windows host.** Extends the desktop onto virtual monitors from the Virtual Display Driver (signed by SignPath; installed with `--install-display-driver`), hardware encoded with zero copy; tested Surface to MacBook Air. Learned the hard way: that driver can't be reconfigured while running (its reload command blocks its only control thread when monitors exist), so the installer fixes 3 monitors and a resolution list, and Windows rejects monitors offering too many modes (keep one global refresh rate). Next:
  - **Test the installer on a clean PC.** The CI-built installer was tested on the Surface (2026-10-12): silent install as administrator put the driver, firewall rule and sign-in entry in place, the tray app started from Program Files, and uninstalling quit it and removed all of them. Still to try on a PC that never had the driver, with the normal (non-silent) installer.
  - The setup dialog only recognises its own firewall rule, not one made by accepting Windows' "allow access" prompt.
  - **Our own SudoVDA-based driver**, signed via SignPath, for true per-client monitors with stable identities (no fixed count or resolution list).
  - Keyboard and mouse mode with Windows as the host (low-level input hooks).

## Next

- **Wi-Fi, part 2.** Done: adaptive bitrate, capped send queue, Windows media streaming mode, link diagnostics on both ends. Still to do:
  - **Measured (Mac on 2.4 GHz, Surface on 5 GHz, 2026-10-12):** 0.0% packet loss, but 10 to 45 frames per 2 s delayed by 65 to 500 ms: Wi-Fi stalls, not loss. So forward error correction and reference frame invalidation (planned earlier; VideoToolbox does accept `EnableLTR` on an M4) wouldn't help on this network; revisit only if a network shows real loss. With both on 5 GHz (after moving the Surface off an access point that isolated its clients), delayed frames all but disappeared. Still worth testing: AWDL off on the Mac.
  - Check how often screens still ask for keyframes in normal use (the host now logs each request; one per connection is expected).
  - Check the adaptive bitrate's thresholds on a busy network (the 2.4 GHz Mac is a good test case).
- **Translations, the rest:** the Wi-Fi tips and network summaries in the details menus, the installers (Inno Setup has Turkish, Spanish, French and German built in; Chinese needs an unofficial file), and the Mac apps' Info.plist text (local network prompt) via `InfoPlist.strings`. Native speakers to review the first translations.
- **Zero copy on Linux clients** (VAAPI surfaces as DMA-BUFs into wgpu's Vulkan backend).

## Later

- **Linux host**: GNOME/KDE/wlroots virtual monitors, PipeWire capture, VAAPI encode.
- **Android client**: Android tablets as extra screens (MediaCodec decoding).
- **Linux packaging**: AppImage or Flatpak (bundling GStreamer). The Mac client app and Windows installer are done.

## Distribution

- **Signing: accounts.** The workflows are ready (see SIGNING.md) and skip signing until the accounts exist: apply to the SignPath Foundation (Windows; also needed for Smart App Control, which blocked an unsigned host build on the Surface), and join the Apple Developer Program (US$99 a year) for Developer ID signing and notarization. Then test a tagged release end to end.
- **Sign the Windows uninstallers** (Inno Setup's `SignedUninstaller` plus one more signing step).
- A disk image background with an arrow, if wanted (needs Finder layout, awkward on CI).
