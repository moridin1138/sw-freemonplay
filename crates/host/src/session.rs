//! One connected client: authenticate, create its virtual display, stream it, apply its input.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fmp_proto::{
    Auth, ClientMsg, DisplayRequest, Hello, HostMsg, InputEvent, PROTOCOL_VERSION, VideoFrame, Welcome, read_msg, write_msg,
};
use tokio::sync::mpsc;
use tokio::time::timeout;

use crate::encoder::{H264Encoder, default_bitrate, fit_stream_size};
use crate::frame::{FrameSlot, Next};
use crate::pattern::Pattern;
use crate::platform::{Capture, Injector, VirtualDisplay};
use crate::store::HostStore;

pub struct Ctx {
    pub store: HostStore,
    pub host_name: String,
    pub max_fps: u32,
    pub bitrate_bps: Option<u32>,
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
    let (mut send, mut recv) = timeout(HANDSHAKE_TIMEOUT, conn.accept_bi()).await.context("no control stream")??;
    let hello = match timeout(HANDSHAKE_TIMEOUT, read_msg::<ClientMsg>(&mut recv)).await.context("no hello")?? {
        Some(ClientMsg::Hello(h)) => h,
        _ => bail!("expected a hello"),
    };
    let who = format!("{} ({})", hello.client_name, conn.remote_address());

    let issued_token = match authenticate(&ctx, &hello) {
        Ok(token) => token,
        Err(reason) => {
            write_msg(&mut send, &HostMsg::Rejected { reason: reason.clone() }).await?;
            send.finish()?;
            // Give the rejection time to arrive before the connection drops.
            let _ = timeout(Duration::from_secs(2), conn.closed()).await;
            bail!("{who} rejected: {reason}");
        }
    };
    if issued_token.is_some() {
        tracing::info!("paired with {who}");
    }

    let req = sanitize(hello.display);
    let fps = ctx.max_fps.min(req.refresh_hz);
    let (width, height) = fit_stream_size(req.width, req.height);
    let bitrate = ctx.bitrate_bps.unwrap_or_else(|| default_bitrate(width, height, fps));
    tracing::info!(
        "{who}: creating {}x{}@{} display (hidpi {}), streaming {width}x{height}@{fps} at {:.1} Mbit/s",
        req.width,
        req.height,
        req.refresh_hz,
        req.hidpi,
        bitrate as f64 / 1e6
    );

    let slot = Arc::new(FrameSlot::default());
    let mut source = if ctx.test_pattern {
        Source::Pattern(Pattern::start(width, height, fps, slot.clone()))
    } else {
        let display_name = format!("freemonplay: {}", hello.client_name);
        let serial = serial_for(&hello.client_id);
        let display = Arc::new(
            tokio::task::spawn_blocking(move || VirtualDisplay::create(&display_name, serial, &req)).await??,
        );
        let capture = {
            let (display, slot) = (display.clone(), slot.clone());
            tokio::task::spawn_blocking(move || Capture::start(&display, width, height, fps, slot)).await??
        };
        let injector = Injector::new(&display);
        Source::Screen { display, capture, injector }
    };

    write_msg(
        &mut send,
        &HostMsg::Welcome(Welcome {
            host_id: ctx.store.host_id.clone(),
            host_name: ctx.host_name.clone(),
            width,
            height,
            refresh_hz: fps,
            issued_token,
        }),
    )
    .await?;
    let mut video = conn.open_uni().await?;

    let (tx, mut rx) = mpsc::channel::<VideoFrame>(2);
    let encoder = {
        let slot = slot.clone();
        std::thread::Builder::new()
            .name("encoder".into())
            .spawn(move || encode_loop(&slot, fps, bitrate, tx))?
    };

    let sender = async {
        let mut stats = Stats::new();
        while let Some(frame) = rx.recv().await {
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

    let result = tokio::select! {
        r = sender => r,
        r = control => r,
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

/// Returns a newly issued token when the client paired with a code, `None` for a known client.
fn authenticate(ctx: &Ctx, hello: &Hello) -> Result<Option<Vec<u8>>, String> {
    if hello.protocol != PROTOCOL_VERSION {
        return Err(format!("protocol version {} not supported (host speaks {PROTOCOL_VERSION})", hello.protocol));
    }
    match &hello.auth {
        Auth::Token(token) if ctx.store.check_token(&hello.client_id, token) => Ok(None),
        Auth::Token(_) => Err("this host doesn't know you any more; pair again with its code".into()),
        Auth::PairingCode(code) => ctx.store.pair(&hello.client_id, &hello.client_name, code).map(Some),
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

fn encode_loop(slot: &FrameSlot, fps: u32, bitrate: u32, tx: mpsc::Sender<VideoFrame>) -> Result<()> {
    let mut encoder = H264Encoder::new(fps, bitrate)?;
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
            pts_us: frame.captured_at.saturating_duration_since(start).as_micros() as u64,
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
