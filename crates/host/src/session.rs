//! One connected client: authenticate, create its virtual display, stream it, apply its input.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::{
    Auth, ClientMsg, Codec, DisplayRequest, Hello, HostMsg, InputEvent, PROTOCOL_VERSION, RejectKind, VideoFrame,
    Welcome,
    decode, peek_hello_protocol, read_frame, read_msg, write_msg,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::encoder::{EncoderChoice, VideoEncoder, default_bitrate, fit_stream_size};
use crate::frame::{FrameSlot, Next};
use crate::pattern::Pattern;
use crate::platform::{Capture, Injector, VirtualDisplay};
use crate::store::HostStore;

pub struct Ctx {
    pub store: HostStore,
    pub host_name: String,
    pub max_fps: u32,
    pub bitrate_bps: Option<u32>,
    pub encoder: EncoderChoice,
    /// Only offer this codec (None: whatever the client prefers that we can encode).
    pub codec: Option<Codec>,
    pub allow_input: bool,
    /// Stream a generated test picture instead of a virtual display.
    pub test_pattern: bool,
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
    let who = format!("{} ({})", hello.client_name, conn.remote_address());

    let issued_token = match authenticate(&ctx, &hello) {
        Ok(token) => token,
        Err((kind, reason)) => return reject(&conn, &mut send, &who, kind, reason).await,
    };
    if issued_token.is_some() {
        tracing::info!("paired with {who}");
    }

    let req = sanitize(hello.display);
    let fps = ctx.max_fps.min(req.refresh_hz);
    let (width, height) = fit_stream_size(req.width, req.height);

    let client_codecs = if hello.codecs.is_empty() { vec![Codec::H264] } else { hello.codecs.clone() };
    let (choice, forced, user_bitrate) = (ctx.encoder, ctx.codec, ctx.bitrate_bps);
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
        let display_name = format!("freemonplay: {}", hello.client_name);
        let serial = serial_for(&hello.client_id);
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

    let (tx, mut rx) = mpsc::channel::<VideoFrame>(2);
    let encoder = {
        let slot = slot.clone();
        std::thread::Builder::new().name("encoder".into()).spawn(move || encode_loop(&slot, encoder, tx))?
    };

    let sender = async {
        let mut stats = Stats::new();
        let started = Instant::now();
        let mut first = true;
        while let Some(frame) = rx.recv().await {
            if std::mem::take(&mut first) {
                tracing::info!("{who}: first frame sent after {} ms", started.elapsed().as_millis());
            }
            stats.add(frame.data.len());
            write_msg(&mut video, &frame).await?;
            stats.maybe_log(&who);
        }
        anyhow::Ok(())
    };
    let control = async {
        loop {
            match read_msg::<ClientMsg>(&mut recv).await? {
                None => return anyhow::Ok(()),
                Some(ClientMsg::Input(event)) if ctx.allow_input => source.inject(&event),
                Some(ClientMsg::RequestKeyframe) => slot.request_keyframe(),
                Some(_) => {}
            }
        }
    };

    // ScreenCaptureKit starts happily without Screen Recording permission and then sends
    // nothing, which would leave the client black with no explanation.
    let watchdog = async {
        tokio::time::sleep(Duration::from_secs(3)).await;
        if !slot.has_frame() {
            let message = "no frames are being captured. Check that the app running the host has Screen \
                           Recording permission (System Settings > Privacy & Security > Screen & System \
                           Audio Recording), then restart it."
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
    write_msg(send, &HostMsg::Rejected { reason: reason.clone(), kind }).await?;
    send.finish()?;
    // Give the rejection time to arrive before the connection drops.
    let _ = timeout(Duration::from_secs(2), conn.closed()).await;
    bail!("{who} rejected: {reason}");
}

/// Returns a newly issued token when the client paired with a code, `None` for a known client.
fn authenticate(ctx: &Ctx, hello: &Hello) -> Result<Option<Vec<u8>>, (RejectKind, String)> {
    match &hello.auth {
        Auth::Token(token) if ctx.store.check_token(&hello.client_id, token) => Ok(None),
        Auth::Token(_) => {
            Err((RejectKind::NotPaired, "this host doesn't know you any more; pair again with its code".into()))
        }
        Auth::PairingCode(code) => {
            ctx.store.pair(&hello.client_id, &hello.client_name, code).map(Some).map_err(|e| (RejectKind::WrongCode, e))
        }
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

fn encode_loop(slot: &FrameSlot, mut encoder: Box<dyn VideoEncoder>, tx: mpsc::Sender<VideoFrame>) -> Result<()> {
    let start = Instant::now();
    let mut need_keyframe = true;
    loop {
        let (frame, keyframe) = match slot.next() {
            Next::Closed => return Ok(()),
            Next::Frame { frame, keyframe } => (frame, keyframe),
        };
        let Some(out) = encoder.encode(&frame, keyframe || need_keyframe)? else { continue };
        need_keyframe = false;
        let video = VideoFrame {
            pts_us: frame.captured_at().saturating_duration_since(start).as_micros() as u64,
            keyframe: out.keyframe,
            data: out.data,
        };
        // Blocks while the network is behind; meanwhile the slot keeps only the newest frame.
        if tx.blocking_send(video).is_err() {
            return Ok(());
        }
    }
}

struct Stats {
    since: Instant,
    frames: u32,
    bytes: usize,
}

impl Stats {
    fn new() -> Self {
        Self { since: Instant::now(), frames: 0, bytes: 0 }
    }
    fn add(&mut self, bytes: usize) {
        self.frames += 1;
        self.bytes += bytes;
    }
    fn maybe_log(&mut self, who: &str) {
        let secs = self.since.elapsed().as_secs_f64();
        if secs >= 10.0 {
            tracing::info!(
                "{who}: {:.0} fps, {:.1} Mbit/s",
                self.frames as f64 / secs,
                self.bytes as f64 * 8.0 / secs / 1e6
            );
            *self = Self::new();
        }
    }
}
