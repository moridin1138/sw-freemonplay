//! One connected client: authenticate, create its virtual display, stream it, apply its input.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::{
    Auth, ClientMsg, Codec, DisplayRequest, Hello, HostMsg, InputEvent, PROTOCOL_VERSION, RejectKind, VideoFrame,
    Welcome,
    decode, pairing, peek_hello_protocol, read_frame, read_msg, write_msg,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::encoder::{CodecChoice, EncoderChoice, VideoEncoder, default_bitrate, fit_stream_size};
use crate::frame::{FrameSlot, Next};
use crate::pattern::Pattern;
use crate::platform::{Capture, Injector, VirtualDisplay};
use crate::rate::{self, RateControl};
use crate::registry::{Registry, SessionGuard, SessionInfo};
use crate::settings::Settings;
use crate::store::HostStore;

/// Everything sessions share. Lives for the whole run.
pub struct Ctx {
    pub store: HostStore,
    pub host_name: String,
    pub port: u16,
    pub endpoint: quinn::Endpoint,
    pub registry: Arc<Registry>,
    /// Withdrawn on shutdown so clients drop us from their lists immediately.
    pub advertisement: Mutex<Option<crate::Advertisement>>,
    /// Saved settings, changed from the menu bar. New sessions pick up changes.
    settings: Mutex<Settings>,
    /// From the command line; win over `settings` for this run and aren't saved.
    pub overrides: Overrides,
    pub bitrate_bps: Option<u32>,
    pub encoder: EncoderChoice,
    /// Stream a generated test picture instead of a virtual display.
    pub test_pattern: bool,
}

#[derive(Default)]
pub struct Overrides {
    pub max_fps: Option<u32>,
    pub codec: Option<CodecChoice>,
    pub no_input: bool,
}

impl Ctx {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: HostStore,
        host_name: String,
        endpoint: quinn::Endpoint,
        overrides: Overrides,
        bitrate_bps: Option<u32>,
        encoder: EncoderChoice,
        test_pattern: bool,
    ) -> anyhow::Result<Self> {
        let settings = Settings::load(store.dir());
        Ok(Self {
            port: endpoint.local_addr()?.port(),
            store,
            host_name,
            endpoint,
            registry: Arc::default(),
            advertisement: Mutex::new(None),
            settings: Mutex::new(settings),
            overrides,
            bitrate_bps,
            encoder,
            test_pattern,
        })
    }

    /// Saved settings with command line overrides applied.
    pub fn settings(&self) -> Settings {
        let mut s = self.settings.lock().unwrap().clone();
        if let Some(fps) = self.overrides.max_fps {
            s.max_fps = fps;
        }
        if let Some(codec) = self.overrides.codec {
            s.codec = codec;
        }
        if self.overrides.no_input {
            s.allow_input = false;
        }
        s
    }

    /// Changes and saves the settings. Affects screens that connect afterwards (input
    /// permission applies immediately).
    pub fn update_settings(&self, change: impl FnOnce(&mut Settings)) {
        let mut s = self.settings.lock().unwrap();
        change(&mut s);
        s.save(self.store.dir());
    }

    /// Disconnects everyone (unplugging their virtual displays) and stops accepting.
    pub fn shutdown(&self) {
        if let Some(ad) = self.advertisement.lock().unwrap().take() {
            ad.withdraw();
        }
        self.endpoint.close(0u32.into(), b"host shutting down");
    }
}

/// Where a session's frames come from, and where its input goes.
enum Source {
    Screen { display: Arc<VirtualDisplay>, capture: Capture, injector: Injector },
    Pattern(Pattern),
}

impl Source {
    fn inject(&mut self, event: &InputEvent) {
        match self {
            Source::Screen { injector, .. } => injector.inject(event),
            Source::Pattern(p) => p.inject(event),
        }
    }
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn run(incoming: quinn::Incoming, ctx: Arc<Ctx>) {
    let remote = incoming.remote_address();
    match handle(incoming, ctx).await {
        Ok(()) => tracing::info!("{remote}: disconnected"),
        Err(e) => tracing::warn!("{remote}: {e:#}"),
    }
}

async fn handle(incoming: quinn::Incoming, ctx: Arc<Ctx>) -> Result<()> {
    let conn = incoming.await?;
    let (mut send, mut recv) = match timeout(HANDSHAKE_TIMEOUT, conn.accept_bi()).await {
        Ok(Ok(stream)) => stream,
        // The client hung up on purpose before starting (e.g. to ask the user for the code).
        Ok(Err(quinn::ConnectionError::ApplicationClosed(_))) => return Ok(()),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => bail!("client connected but didn't start a session within {} s", HANDSHAKE_TIMEOUT.as_secs()),
    };
    let body = timeout(HANDSHAKE_TIMEOUT, read_frame(&mut recv)).await.context("no hello")??.context("no hello")?;
    // Check the version first: a hello from another version won't deserialize.
    match peek_hello_protocol(&body) {
        Some(PROTOCOL_VERSION) => {}
        Some(v) => {
            let newer = if v > PROTOCOL_VERSION { "host" } else { "client" };
            let reason = format!("protocol version {v} vs host's {PROTOCOL_VERSION}; update the {newer}");
            let who = conn.remote_address().to_string();
            return reject(&conn, &mut send, &who, RejectKind::VersionMismatch, reason).await;
        }
        None => bail!("expected a hello"),
    }
    let ClientMsg::Hello(hello) = decode(&body).context("malformed hello")? else {
        bail!("expected a hello");
    };
    // Each of a client's monitors is a session of its own, named after the monitor.
    let screen_name = match &hello.screen {
        Some(screen) => format!("{} ({})", hello.client_name, screen.name),
        None => hello.client_name.clone(),
    };
    let who = format!("{screen_name} ({})", conn.remote_address());

    let issued_token = match &hello.auth {
        Auth::Token(token) if ctx.store.check_token(&hello.client_id, token) => None,
        Auth::Token(_) => {
            let reason = "this host doesn't know you any more; pair again with its code".to_string();
            return reject(&conn, &mut send, &who, RejectKind::NotPaired, reason).await;
        }
        Auth::Pake(client_msg) => match pair(&ctx, &conn, &mut send, &mut recv, client_msg, &hello).await {
            Ok(token) => Some(token),
            Err(e) => {
                ctx.store.pairing_failed();
                tracing::debug!("{who}: pairing failed: {e:#}");
                return reject(&conn, &mut send, &who, RejectKind::WrongCode, "wrong pairing code".into()).await;
            }
        },
    };
    if issued_token.is_some() {
        tracing::info!("paired with {who}");
    }
    if let Some(network) = crate::platform::network::describe() {
        tracing::info!("this Mac's network: {}", network.summary);
        for tip in &network.advice {
            tracing::info!("network tip: {tip}");
        }
    }

    let req = sanitize(hello.display);
    let settings = ctx.settings();
    let fps = settings.max_fps.clamp(1, 240).min(req.refresh_hz);
    let (width, height) = fit_stream_size(req.width, req.height);

    let client_codecs = if hello.codecs.is_empty() { vec![Codec::H264] } else { hello.codecs.clone() };
    let (choice, forced, user_bitrate) = (ctx.encoder, settings.codec.only(), ctx.bitrate_bps);
    let negotiated = tokio::task::spawn_blocking(move || {
        crate::encoder::negotiate(choice, forced, &client_codecs, (width, height, fps), |codec| {
            user_bitrate.unwrap_or_else(|| default_bitrate(codec, width, height, fps))
        })
    })
    .await?;
    let (encoder, codec, bitrate) = match negotiated {
        Ok(n) => n,
        Err(e) => return reject(&conn, &mut send, &who, RejectKind::NoCommonCodec, format!("{e:#}")).await,
    };
    tracing::info!(
        "{who}: creating {}x{}@{} display (hidpi {}), streaming {width}x{height}@{fps} {codec} at {:.1} Mbit/s with {}",
        req.width,
        req.height,
        req.refresh_hz,
        req.hidpi,
        bitrate as f64 / 1e6,
        encoder.name(),
    );

    let slot = Arc::new(FrameSlot::default());
    let mut source = if ctx.test_pattern {
        Source::Pattern(Pattern::start(width, height, fps, slot.clone()))
    } else {
        let display_name = format!("freemonplay: {screen_name}");
        let serial = match &hello.screen {
            Some(screen) => serial_for(&format!("{}/{}", hello.client_id, screen.key)),
            None => serial_for(&hello.client_id),
        };
        match start_screen(display_name, serial, req, (width, height, fps), slot.clone()).await {
            Ok(source) => source,
            // Tell the client what went wrong rather than just dropping the connection.
            Err(e) => return reject(&conn, &mut send, &who, RejectKind::Other, format!("host error: {e:#}")).await,
        }
    };

    write_msg(
        &mut send,
        &HostMsg::Welcome(Welcome {
            host_id: ctx.store.host_id.clone(),
            host_name: ctx.host_name.clone(),
            width,
            height,
            refresh_hz: fps,
            codec,
            issued_token,
        }),
    )
    .await?;
    let mut video = conn.open_uni().await?;
    let session = ctx.registry.add(
        SessionInfo {
            id: 0,
            client_name: screen_name.clone(),
            addr: conn.remote_address(),
            width,
            height,
            codec,
            encoder: encoder.name(),
            fps: 0.0,
            mbps: 0.0,
            host_ms: 0.0,
            rtt_ms: 0.0,
            target_mbps: bitrate as f32 / 1e6,
            max_mbps: bitrate as f32 / 1e6,
            loss_pct: 0.0,
            network: None,
            client: None,
        },
        conn.clone(),
    );

    // Frames travel with their capture time, to measure capture-to-sent latency.
    let (tx, mut rx) = mpsc::channel::<(VideoFrame, Instant)>(2);
    // The sender lowers this when the network can't keep up; the encoder applies it.
    let target_bitrate = Arc::new(AtomicU32::new(bitrate));
    let encoder = {
        let (slot, target) = (slot.clone(), target_bitrate.clone());
        std::thread::Builder::new().name("encoder".into()).spawn(move || encode_loop(&slot, encoder, tx, &target))?
    };

    let sender = async {
        let mut stats = Stats::new();
        let started = Instant::now();
        let mut first = true;
        let mut rate = RateControl::new(bitrate, Instant::now());
        conn.set_send_window(rate::send_window(bitrate, conn.rtt()));
        let (mut tick, mut stalled) = (Instant::now(), Duration::ZERO);
        while let Some((frame, captured_at)) = rx.recv().await {
            if std::mem::take(&mut first) {
                tracing::info!("{who}: first frame sent after {} ms", started.elapsed().as_millis());
            }
            let (bytes, keyframe) = (frame.data.len(), frame.keyframe);
            let write_started = Instant::now();
            write_msg(&mut video, &frame).await?;
            if !keyframe {
                stalled += write_started.elapsed();
            }
            stats.add(bytes, captured_at.elapsed());
            stats.maybe_report(&who, &session, conn.rtt());

            if tick.elapsed() >= rate::TICK {
                let path = conn.stats().path;
                let sample = rate::Sample {
                    elapsed: tick.elapsed(),
                    stalled,
                    sent_packets: path.sent_packets,
                    lost_packets: path.lost_packets,
                };
                if let Some(bps) = rate.update(Instant::now(), &sample) {
                    tracing::info!("{who}: bitrate {:.1} Mbit/s (network {})", bps as f64 / 1e6,
                        if bps < target_bitrate.load(Ordering::Relaxed) { "behind" } else { "keeping up" });
                    target_bitrate.store(bps, Ordering::Relaxed);
                }
                conn.set_send_window(rate::send_window(rate.current(), path.rtt));
                session.update_rate(rate.current(), rate.max(), rate.loss());
                (tick, stalled) = (Instant::now(), Duration::ZERO);
            }
        }
        anyhow::Ok(())
    };
    let control = async {
        loop {
            match read_msg::<ClientMsg>(&mut recv).await? {
                None => return anyhow::Ok(()),
                Some(ClientMsg::Input(event)) if ctx.settings().allow_input => source.inject(&event),
                Some(ClientMsg::RequestKeyframe) => slot.request_keyframe(),
                Some(ClientMsg::Network(network)) => {
                    tracing::info!("{who}: network: {}", network.summary);
                    session.update_network(network);
                }
                Some(ClientMsg::Stats(stats)) => {
                    session.update_client(stats);
                    tracing::debug!("{who}: screen reports {stats:?}");
                }
                Some(_) => {}
            }
        }
    };

    // ScreenCaptureKit starts happily without Screen Recording permission and then sends
    // nothing, which would leave the client black with no explanation.
    let watchdog = async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if !slot.has_frame() {
            let message = "no frames are being captured. Check that freemonplay (or the terminal running \
                           it) has Screen Recording permission in System Settings > Privacy & Security > \
                           Screen & System Audio Recording, then restart it. If it's already on, remove \
                           the entry and allow it again: it may belong to an older build."
                .to_string();
            tracing::warn!("{who}: {message}");
            write_msg(&mut send, &HostMsg::Notice { message }).await?;
        }
        std::future::pending::<()>().await;
        anyhow::Ok(())
    };

    let result = tokio::select! {
        r = sender => r,
        r = control => r,
        r = watchdog => r,
        _ = conn.closed() => Ok(()),
    };

    slot.close();
    match encoder.join() {
        Ok(Err(e)) => tracing::error!("encoder failed: {e:#}"),
        Err(_) => tracing::error!("encoder thread panicked"),
        Ok(Ok(())) => {}
    }
    // Stop capture before unplugging the display it captures.
    if let Source::Screen { display, capture, .. } = source {
        drop(capture);
        drop(display);
    }
    result
}

/// Creates the client's virtual display and starts capturing it.
async fn start_screen(
    name: String,
    serial: u32,
    req: DisplayRequest,
    (width, height, fps): (u32, u32, u32),
    slot: Arc<FrameSlot>,
) -> Result<Source> {
    let display = Arc::new(
        tokio::task::spawn_blocking(move || VirtualDisplay::create(&name, serial, &req))
            .await?
            .context("creating the virtual display")?,
    );
    let capture = {
        let display = display.clone();
        tokio::task::spawn_blocking(move || Capture::start(&display, width, height, fps, slot))
            .await?
            .context("starting screen capture")?
    };
    let injector = Injector::new(&display);
    Ok(Source::Screen { display, capture, injector })
}

/// Tells the client why it can't connect, then ends the session with that reason.
async fn reject(
    conn: &quinn::Connection,
    send: &mut quinn::SendStream,
    who: &str,
    kind: RejectKind,
    reason: String,
) -> Result<()> {
    // Best effort: the client may already have hung up (e.g. it rejected our pairing proof).
    let _ = write_msg(send, &HostMsg::Rejected { reason: reason.clone(), kind }).await;
    let _ = send.finish();
    // Give the rejection time to arrive before the connection drops.
    let _ = timeout(Duration::from_secs(2), conn.closed()).await;
    bail!("{who} rejected: {reason}");
}

/// Pairing steps 2 to 4 (see `fmp_proto::pairing`): answer the client's SPAKE2 message,
/// prove we know the code, and check the client's proof. Returns the client's new token.
async fn pair(
    ctx: &Ctx,
    conn: &quinn::Connection,
    send: &mut quinn::SendStream,
    recv: &mut quinn::RecvStream,
    client_msg: &[u8],
    hello: &Hello,
) -> Result<Vec<u8>> {
    let (state, host_msg) = pairing::Pairing::host(&ctx.store.pairing_code());
    let key = state.finish(client_msg)?;
    let binding = pairing::channel_binding(conn)?;
    let proof = key.confirmation(pairing::Role::Host, &binding);
    write_msg(send, &HostMsg::PairingChallenge { pake: host_msg, proof }).await?;
    match timeout(HANDSHAKE_TIMEOUT, read_msg::<ClientMsg>(recv)).await {
        Ok(Ok(Some(ClientMsg::PairingProof(proof)))) if key.verify(pairing::Role::Client, &binding, &proof) => {
            Ok(ctx.store.complete_pairing(&hello.client_id, &hello.client_name))
        }
        Ok(Ok(Some(ClientMsg::PairingProof(_)))) => bail!("client's proof doesn't match"),
        Ok(Ok(_)) | Ok(Err(_)) => bail!("client gave up (wrong code, or an interrupted connection)"),
        Err(_) => bail!("client didn't answer in time"),
    }
}

fn sanitize(mut r: DisplayRequest) -> DisplayRequest {
    r.width = r.width.clamp(320, 8192) & !1;
    r.height = r.height.clamp(240, 8192) & !1;
    r.refresh_hz = if (24..=240).contains(&r.refresh_hz) { r.refresh_hz } else { 60 };
    if r.hidpi && r.width.min(r.height) < 1200 {
        // Too small for 2x to leave a usable desktop (1080p at 2x would be 960x540).
        r.hidpi = false;
    }
    r
}

/// Stable per-client serial number (FNV-1a), so macOS remembers each screen's arrangement.
fn serial_for(client_id: &str) -> u32 {
    client_id.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193))
}

fn encode_loop(
    slot: &FrameSlot,
    mut encoder: Box<dyn VideoEncoder>,
    tx: mpsc::Sender<(VideoFrame, Instant)>,
    target_bitrate: &AtomicU32,
) -> Result<()> {
    crate::platform::prioritize_current_thread();
    let start = Instant::now();
    let mut need_keyframe = true;
    let mut bitrate = target_bitrate.load(Ordering::Relaxed);
    loop {
        let (frame, keyframe) = match slot.next() {
            Next::Closed => return Ok(()),
            Next::Frame { frame, keyframe } => (frame, keyframe),
        };
        let wanted = target_bitrate.load(Ordering::Relaxed);
        if wanted != bitrate {
            if let Err(e) = encoder.set_bitrate(wanted) {
                tracing::warn!("couldn't change the bitrate: {e:#}");
            }
            bitrate = wanted;
        }
        let Some(out) = encoder.encode(&frame, keyframe || need_keyframe)? else { continue };
        need_keyframe = false;
        let captured_at = frame.captured_at();
        let video = VideoFrame {
            pts_us: captured_at.saturating_duration_since(start).as_micros() as u64,
            keyframe: out.keyframe,
            data: out.data,
        };
        // Blocks while the network is behind; meanwhile the slot keeps only the newest frame.
        if tx.blocking_send((video, captured_at)).is_err() {
            return Ok(());
        }
    }
}

/// Frame rate, bitrate and capture-to-sent time over 2 s windows: shown live in the menu,
/// logged every 10 s.
struct Stats {
    since: Instant,
    frames: u32,
    bytes: usize,
    host_latency: Duration,
    windows: u32,
}

impl Stats {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0, bytes: 0, host_latency: Duration::ZERO, windows: 0 }
    }
    fn add(&mut self, bytes: usize, host_latency: Duration) {
        self.frames += 1;
        self.bytes += bytes;
        self.host_latency += host_latency;
    }
    fn maybe_report(&mut self, who: &str, session: &SessionGuard, rtt: Duration) {
        let secs = self.since.elapsed().as_secs_f64();
        if secs < 2.0 {
            return;
        }
        let fps = (self.frames as f64 / secs) as f32;
        let mbps = (self.bytes as f64 * 8.0 / secs / 1e6) as f32;
        let host_ms = (self.host_latency.as_secs_f64() * 1e3 / self.frames.max(1) as f64) as f32;
        let rtt_ms = (rtt.as_secs_f64() * 1e3) as f32;
        session.update_stats(fps, mbps, host_ms, rtt_ms);
        self.windows += 1;
        if self.windows.is_multiple_of(5) {
            tracing::info!("{who}: {fps:.0} fps, {mbps:.1} Mbit/s, host {host_ms:.1} ms, round trip {rtt_ms:.1} ms");
        }
        *self = Self { windows: self.windows, ..Self::new() };
    }
}
