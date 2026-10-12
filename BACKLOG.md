# Backlog

Roughly in priority order. Done items move to [CHANGELOG.md](CHANGELOG.md).

## In progress

- **Several monitors on one client.** Built (protocol 5): each extra monitor is its own session and its own screen on the host. Needs testing with real extra monitors on Windows, and on Mac and Linux clients.
- **Keep client screens awake.** Tested on macOS. Windows (`SetThreadExecutionState`) and Linux (D-Bus ScreenSaver inhibit, with GNOME session manager fallback) still need checking on real machines.

## Next

- **Better behaviour on Wi-Fi.** The network is most of the latency: a Surface on home Wi-Fi measured a 74 ms round trip (37 of about 54 ms in total). Ideas: detect and suggest turning off Wi-Fi power saving; adaptive bitrate from loss and round-trip feedback; stop one lost packet holding up later frames (per-frame streams, or datagrams with keyframe recovery).
- **Zero copy on Linux clients** (VAAPI surfaces as DMA-BUFs into wgpu's Vulkan backend).

## Later

- **Keyboard & mouse only mode (software KVM).** Use the Mac's keyboard and mouse on another computer's own screens without streaming video, like Universal Control or Synergy. Moving the pointer off an edge of the Mac's screen hands input to the other computer; moving back returns it.
  - Mac side: watch the pointer reaching a configured screen edge, capture keyboard and mouse with an event tap (Accessibility permission), hide the local cursor, and send relative movement and keys.
  - Client side: inject input into its own OS (Windows `SendInput`; Linux libei or uinput; macOS CGEvent), and position the pointer at the matching edge.
  - **Copy and paste in both directions:** sync the clipboard (text first, then images and files) when focus crosses machines.
  - Screen layout setting: which edge of which screen leads to which computer.
  - **Mixed with several monitors:** each of a client's monitors is either a *screen* (streamed from the Mac) or *keyboard and mouse only* (the client's own desktop, controlled from the Mac). Example: a PC with 3 monitors lends one to the Mac as a screen (two screens on the Mac in total) and its other two stay the PC's own, reached by moving the Mac's pointer off the edge of that streamed screen. So the edges that lead to the client follow the client's physical monitor layout, starting from the screen the Mac streams to it.
  - Client UI: one choice per monitor (Screen / Keyboard and mouse / Not used) instead of today's checkboxes.
- **Windows host** (extra screens for a Windows PC): bundle an Indirect Display Driver, capture with Windows.Graphics.Capture, hardware encode with Media Foundation/NVENC.
- **Linux host**: GNOME/KDE/wlroots virtual monitors, PipeWire capture, VAAPI encode.
- **Android client**: Android tablets as extra screens (MediaCodec decoding).
- **Client packaging**: Mac client `.app`, Windows installer, Linux desktop entry and AppImage/Flatpak.

## Housekeeping

- Bump `actions/checkout` and `actions/upload-artifact`/`download-artifact` to their Node 24 versions (GitHub warns that Node 20 actions are deprecated).
