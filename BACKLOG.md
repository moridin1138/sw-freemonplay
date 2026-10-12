# Backlog

Roughly in priority order. Done items move to the release notes.

## In progress

- **Windows zero-copy display.** Media Foundation decodes on the GPU; frames are copied GPU-to-GPU into shared NV12 textures and drawn by wgpu's Direct3D 12 backend, synchronised with a shared fence. Falls back to copying on any failure (`FMP_ZERO_COPY=0` forces that). Passes `--self-test` on a Surface (Iris Xe): decode 1.8 ms per frame versus 4.9 ms copying. Still to check: lag in a real session at full resolution.

- **Keep client screens awake.** Done on macOS (tested). Windows (`SetThreadExecutionState`) and Linux (D-Bus ScreenSaver inhibit, with GNOME session manager fallback) need testing on real machines.

## Next

- **Several monitors on one client.** Per-monitor identity in the handshake (stable serial and name per monitor, so macOS remembers each arrangement) and a client option to use several monitors at once.
- **Better behaviour on Wi-Fi.** Adaptive bitrate from loss/round-trip feedback; stop one lost packet holding up later frames (per-frame streams or datagrams with keyframe recovery).

## Later

- **Keyboard & mouse only mode (software KVM).** Use the Mac's keyboard and mouse on another computer's own screens without streaming video, like Universal Control or Synergy. Moving the pointer off an edge of the Mac's screen hands input to the other computer; moving back returns it.
  - Mac side: watch the pointer reaching a configured screen edge, capture keyboard and mouse with an event tap (Accessibility permission), hide the local cursor, and send relative movement and keys.
  - Client side: inject input into its own OS (Windows `SendInput`; Linux libei or uinput; macOS CGEvent), and position the pointer at the matching edge.
  - **Copy and paste in both directions:** sync the clipboard (text first, then images and files) when focus crosses machines.
  - Screen layout setting: which edge of which screen leads to which computer.
- **Windows host** (extra screens for a Windows PC): bundle an Indirect Display Driver, capture with Windows.Graphics.Capture, hardware encode with Media Foundation/NVENC.
- **Linux host**: GNOME/KDE/wlroots virtual monitors, PipeWire capture, VAAPI encode.
- **Android client**: Android tablets as extra screens (MediaCodec decoding).
- **Client packaging**: Mac client `.app`, Windows installer, Linux desktop entry and AppImage/Flatpak.

## Housekeeping

- Bump `actions/checkout` and `actions/upload-artifact`/`download-artifact` to their Node 24 versions (GitHub warns that Node 20 actions are deprecated).
