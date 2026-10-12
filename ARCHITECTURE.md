# Architecture

How freemonplay is put together, for anyone changing it. The README covers building and
using it; [CHANGELOG.md](CHANGELOG.md) covers what changed when; [BACKLOG.md](BACKLOG.md)
covers what's next.

## The big picture

```
 Host (Mac)                                              Client (Mac / Windows / Linux)
 ┌─────────────────────────────────────┐   QUIC, one     ┌──────────────────────────────────┐
 │ virtual display (CGVirtualDisplay)  │   connection    │ window per screen (winit)        │
 │   → capture (ScreenCaptureKit)      │   per screen    │   ← render (wgpu, NV12 → RGB)    │
 │   → encode (VideoToolbox)           │ ─── video ────▶ │   ← decode (OS decoder)          │
 │   ← input injection (CGEvent)       │ ◀── control ─── │   → input, stats, keyframe asks  │
 └─────────────────────────────────────┘                 └──────────────────────────────────┘
```

The **host** is the computer that wants more screens. For every screen that connects, it
creates a real virtual monitor, captures it, hardware encodes it and streams it. The
**client** shows the stream full screen and sends input back. One session is one screen:
a client using several of its monitors runs several sessions.

## Crates

```
crates/
  proto/    fmp-proto: wire protocol shared by both sides (messages, framing, pairing)
  host/     freemonplay-host: mDNS advert, QUIC server, sessions, menu bar app
    session.rs        one connection: handshake, pairing, display, encode loop, input
    rate.rs           adaptive bitrate and the send window
    registry.rs       live sessions, for the menu bar (stats, Disconnect)
    store.rs          host id, pairing code, paired clients (token hashes)
    encoder.rs        codec negotiation; OpenH264 software fallback
    frame.rs          FrameSlot: newest-frame-wins handoff from capture to encoder
    menubar.rs        the macOS menu bar app (objc2-app-kit)
    platform/macos/   virtual_display.rs, capture.rs, videotoolbox.rs, input.rs,
                      network.rs (this Mac's Wi-Fi, via CoreWLAN)
    platform/unsupported.rs   stubs (and the plan) for Windows and Linux hosts
  client/   freemonplay-client: discovery, sessions, decoding, rendering, input, GUI
    app.rs            the app state machine and its windows ("displays")
    ui.rs             egui screens: host list, pairing code, connecting
    net.rs            discovery (mDNS), connecting, the session loop
    decode/           videotoolbox.rs (macOS), mediafoundation.rs (Windows),
                      gstreamer.rs (Linux), software.rs (OpenH264 last resort)
    render.rs         wgpu renderer: YUV planes to the screen, plus egui
    render_metal.rs   macOS zero copy: decoder buffers as Metal textures
    render_d3d12.rs   Windows zero copy: shared D3D textures in wgpu's D3D12 device
    awake.rs          keep the screen on while connected
    link.rs           the network link: Wi-Fi tuning and description (Windows)
    input.rs          winit input to protocol events (HID usages, normalised coords)
    selftest.rs       --self-test: decode and draw a built-in clip without a window
    store.rs          client id, paired hosts (token, pinned certificate fingerprint)
```

## Protocol

QUIC (`quinn`, TLS via `rustls` with a self-signed host certificate). Messages are
`postcard`, each with a little-endian `u32` length prefix (`proto/src/lib.rs`).

Per connection:
- **Control stream** (bidirectional, opened by the client): `Hello`, then pairing messages
  if needed, then `Welcome` or `Rejected`. After that the client sends `Input`, `Stats`,
  `RequestKeyframe` and `Network` (its link, once known); the host sends `Notice`s.
- **Video stream** (unidirectional, host to client): `VideoFrame`s, each one access unit in
  Annex-B form with its capture timestamp and a keyframe flag.

`Hello` carries the protocol version, the client's stable id and name, its auth (pairing
or token), the display it wants (size, refresh, HiDPI), the codecs it decodes (preferred
first) and, for extra monitors, a `ScreenId`.

**Versioning.** `PROTOCOL_VERSION` is bumped whenever a message changes shape. `Hello`'s
first field is always the version, so the host can read it from any client
(`peek_hello_protocol`) and reject a mismatch with `RejectKind::VersionMismatch` and a
message saying which side to update. Clients only forget a pairing on
`RejectKind::NotPaired`.

## A session, start to finish

**Host** (`host/src/session.rs`):
1. Accept the connection and the control stream; read and check `Hello`.
2. Authenticate: a known token, or run pairing (below) and issue a token.
3. Negotiate the codec and bitrate (`encoder::negotiate`).
4. Create the virtual display (name and serial from the client's identity) and start
   capture into a `FrameSlot`.
5. Send `Welcome`, open the video stream, register the session for the menu bar.
6. Run until any side stops: the encoder thread, the sender, the control reader (input,
   stats, keyframe requests) and a watchdog that sends a `Notice` if no frames arrive.
7. Dropping the session tears down capture and the virtual display.

**Client** (`client/src/net.rs`, `app.rs`):
1. `prepare`: resolve and connect; decide between a saved token and asking for the code.
2. `run`: send `Hello`, finish pairing if needed, receive `Welcome`, start the decoder
   thread, then read video and write control messages until the connection ends.
3. Events go to the UI thread through winit's `EventLoopProxy`, tagged with the display
   they belong to: `Prepared`, `Connected`, `Frame`, `Closed`.

## Threads and the frame path

```
Host:   ScreenCaptureKit callback ─▶ FrameSlot ─▶ encoder thread ─▶ mpsc(2) ─▶ async sender ─▶ QUIC
Client: QUIC reader ─▶ sync_channel(4) ─▶ decoder thread ─▶ FrameSlot ─▶ UI thread: upload + present
```

Latency beats completeness everywhere:
- Both `FrameSlot`s keep only the newest frame. If encoding or display falls behind,
  older frames are skipped rather than queued.
- If the client's decoder queue is full, the reader drops frames until the next keyframe
  and asks the host for one.
- The client draws as soon as a frame is decoded (`UserEvent::Frame`), not on the next
  redraw tick.

## Adapting to the network

`host/src/rate.rs`. After a 2 s grace period (the first keyframe and screen paint are
bursts), every 500 ms the sender feeds a `RateControl` with how long writes to
QUIC stalled (keyframes excluded: they're big by design), the bytes it sent, and quinn's
sent and lost packet counts. A tick is stalled when writes waited more than 20% of it,
but stalls count as congestion only when the link was busy (sending at least 60% of the
current bitrate) or stalling has gone on for 2 s: Wi-Fi also stalls for 100 to 500 ms at
a time with no loss (airtime contention, AWDL channel hopping), and a lower bitrate
doesn't help with those. Congestion, or more than 5% of packets lost, cuts the bitrate by
30% (at most once a second, down to an eighth of the maximum or 1.5 Mbit/s);
3 s without trouble raises it by 10%, up to the negotiated maximum. The encoder thread
applies changes between frames (`VideoEncoder::set_bitrate`; VideoToolbox supports it
mid-stream, OpenH264 keeps its rate).

Stalls only show up if QUIC can't buffer unlimited video, so the connection's send
window is kept at about the current bitrate times (round trip + 60 ms), between 128 KB
and 4 MB (`rate::send_window`). A link slower than the bitrate then stalls the sender
within a frame or two, the encoder skips frames (newest wins) and the bitrate drops,
instead of seconds of video queueing inside QUIC.

Both ends describe their link for the menu: the client sends `ClientMsg::Network`
(Windows: the interface that routes to the host, via the WLAN API, which also switches on
media streaming mode for the session); the host reads its own Wi-Fi with CoreWLAN and
checks whether AWDL is active.

## Zero copy

Frames stay on the GPU from capture to screen where the platform allows:
- **Host (macOS):** ScreenCaptureKit delivers NV12 `IOSurface`-backed buffers that go
  straight into VideoToolbox.
- **Mac client:** VideoToolbox's output buffers are wrapped as Metal textures through a
  `CVMetalTextureCache` and imported into wgpu (`render_metal.rs`).
- **Windows client:** Media Foundation decodes on the GPU (DXVA). Each picture is copied
  GPU to GPU into one of 4 shared NV12 textures, and a shared fence is signalled. The
  renderer opens those in wgpu's D3D12 device and makes its queue wait on the fence
  before drawing (`render_d3d12.rs`). The decoder runs on the renderer's GPU (adapter
  LUID). A frame holds a lease on its texture slot until it's replaced on screen; if all
  slots are busy, that frame is copied instead.
- **Linux client:** copies through system memory (for now).

Any import failure switches that display to copying for the rest of the session.
`FMP_ZERO_COPY=0` forces copying. `--self-test` exercises both paths without a window.

## Several monitors on one client

Each extra monitor is an independent session with its own window, renderer, decoder and
QUIC connection (`app.rs`: `Display`, keyed by a `view` number; `0` is the main window).
The main window connects (and pairs) first; the extras then connect with the saved token.
Their `Hello` carries a `ScreenId` (the monitor's OS name), from which the host derives a
separate display name and serial number. The main window sends no `ScreenId`, so it keeps
the client's original identity and macOS arrangement. Disconnecting any screen ends them
all; an extra failing on its own just closes that window.

## Pairing and trust

The first connection runs SPAKE2 (Ed25519 group) keyed by the host's 6 digit code, then
both sides prove they derived the same key with an HMAC over a TLS exporter value
(`EXPORTER-freemonplay-pairing-v1`), binding the proof to this TLS connection. The code
never crosses the network, an attacker gets one online guess per attempt (five failures
rotate the code), and a relayed connection fails even with the right code. The host then
issues a random token and stores only its hash; the client stores the token and pins the
host's certificate fingerprint (`proto/src/pairing.rs`, `host/src/store.rs`,
`client/src/store.rs`).

## Discovery

The host advertises `_freemonplay._udp` over mDNS (IPv4 only; loopback and IPv6 off) and
withdraws it on shutdown. The client browses continuously and ranks each host's addresses
(private LAN first, then CGNAT, link-local, public IPv6). **Connect by address** skips
discovery.

## Latency measurement

Each side measures on its own clock: the host from capture to sent, the client from a
frame fully arriving to it being presented (matched through the decoder by timestamp), and
QUIC tracks the round trip. The client reports every 2 s (`Stats`); the host shows host +
half the round trip + client in the menu. Capture delay and the wait for the client's next
refresh aren't included, so the real figure is somewhat higher.

## Platform notes

- **macOS host:** `CGVirtualDisplay` is private API, called through the objc2 runtime.
  HiDPI modes are given in points, `maxPixels` in pixels. The app holds an
  `NSProcessInfo` activity so App Nap and display sleep don't stall capture, and the
  encoder thread runs at user-interactive QoS. Screen Recording and Accessibility
  permissions are tied to the code signature; see `build/macos/`.
- **Keep awake (client):** Windows `SetThreadExecutionState`, macOS `NSProcessInfo`
  activity, Linux D-Bus `org.freedesktop.ScreenSaver.Inhibit` with GNOME's session
  manager as fallback.

## Testing

- `cargo test --workspace`: protocol, address ranking, pairing, clip parsing.
- `freemonplay-client --self-test`: real decoder and renderer, off screen; checks
  colours, timestamps and which path ran (zero copy or copying). Works over SSH.
- `freemonplay-host --test-pattern`: streams a generated picture, no permissions needed.
- CI (`.github/workflows/ci.yml`) builds the Mac app and the clients for all three OSes;
  `release.yml` publishes them for `v*` tags.
