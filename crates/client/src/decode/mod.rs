//! Video decoding (H.264, and HEVC where the platform can) on its own thread, handing the
//! newest picture to the renderer.
//!
//! Uses the platform's own decoder so the client ships no H.264 code of its own:
//! - macOS: VideoToolbox (hardware)
//! - Windows: Media Foundation, with DXVA hardware decoding when a D3D11 GPU is available
//! - Linux: GStreamer, preferring the VAAPI hardware decoder; bundled OpenH264 as a last resort

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::Result;
use fmp_proto::{ClientMsg, Codec, VideoFrame};
use tokio::sync::mpsc::UnboundedSender;
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

mod annexb;
#[cfg(target_os = "linux")]
mod gstreamer;
#[cfg(target_os = "windows")]
pub mod mediafoundation;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod software;
#[cfg(target_os = "macos")]
pub mod videotoolbox;

/// A decoded 4:2:0 picture. Planes keep the decoder's row padding (strides in bytes).
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    pub planes: Planes,
    /// The source frame's timestamp, carried through the decoder.
    pub pts_us: u64,
    /// When the source frame finished arriving (filled in by `run`), for latency stats.
    pub received_at: Option<Instant>,
}

pub enum Planes {
    /// Separate U and V planes (the software decoder).
    #[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
    I420 { y: Vec<u8>, u: Vec<u8>, v: Vec<u8>, y_stride: u32, uv_stride: u32 },
    /// One interleaved CbCr plane (what hardware decoders produce).
    Nv12 { y: Vec<u8>, uv: Vec<u8>, y_stride: u32, uv_stride: u32 },
    /// The decoder's own GPU-shareable NV12 buffer, drawn without copying (macOS).
    #[cfg(target_os = "macos")]
    Native(videotoolbox::PixelBuffer),
    /// An NV12 texture on the GPU shared with the renderer, drawn without copying (Windows).
    #[cfg(target_os = "windows")]
    Shared(mediafoundation::SharedFrame),
}

impl Planes {
    /// How the picture reaches the renderer, for diagnostics.
    pub fn describe(&self) -> &'static str {
        match self {
            Planes::I420 { .. } => "I420 in system memory",
            Planes::Nv12 { .. } => "NV12 in system memory",
            #[cfg(target_os = "macos")]
            Planes::Native(_) => "VideoToolbox GPU buffer",
            #[cfg(target_os = "windows")]
            Planes::Shared(_) => "shared Direct3D texture",
        }
    }
}

/// Where decoders hand finished pictures. Some decoders call it from their own threads.
pub type Deliver = Arc<dyn Fn(YuvFrame) + Send + Sync>;

pub trait Decoder {
    fn name(&self) -> String;
    /// Feeds one access unit (Annex-B). Pictures come out through `Deliver`, possibly later,
    /// carrying `pts_us` so they can be matched back to their source frame.
    fn decode(&mut self, access_unit: &[u8], pts_us: u64) -> Result<()>;
}

/// Codecs this machine can decode, most preferred first. HEVC goes first only when the
/// GPU decodes it; in software it costs more CPU than H.264 for no visible gain on a LAN.
pub fn supported_codecs() -> Vec<Codec> {
    #[cfg(target_os = "macos")]
    return videotoolbox::supported_codecs();
    #[cfg(target_os = "windows")]
    return mediafoundation::supported_codecs();
    #[cfg(target_os = "linux")]
    return gstreamer::supported_codecs();
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    return vec![Codec::H264];
}

/// `gpu_adapter` identifies the GPU the renderer uses (Windows: its LUID), so a hardware
/// decoder on the same GPU can hand frames over without copying.
pub(crate) fn new_decoder(codec: Codec, deliver: Deliver, gpu_adapter: Option<u64>) -> Result<Box<dyn Decoder>> {
    #[cfg(not(target_os = "windows"))]
    let _ = gpu_adapter;
    #[cfg(target_os = "macos")]
    return Ok(Box::new(videotoolbox::VtDecoder::new(codec, deliver)));
    #[cfg(target_os = "windows")]
    return Ok(Box::new(mediafoundation::MfDecoder::new(codec, deliver, gpu_adapter)?));
    #[cfg(target_os = "linux")]
    return match gstreamer::GstDecoder::new(codec, deliver.clone()) {
        Ok(d) => Ok(Box::new(d) as Box<dyn Decoder>),
        Err(e) if codec == Codec::H264 => {
            tracing::warn!("GStreamer decoding unavailable ({e:#}); falling back to bundled OpenH264");
            Ok(Box::new(software::SoftwareDecoder::new(deliver)?))
        }
        Err(e) => Err(e),
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    return {
        anyhow::ensure!(codec == Codec::H264, "only H.264 is supported here");
        Ok(Box::new(software::SoftwareDecoder::new(deliver)?))
    };
}

/// Holds only the newest decoded picture; the renderer takes it on redraw.
#[derive(Default)]
pub struct FrameSlot(Mutex<Option<YuvFrame>>);

impl FrameSlot {
    /// Returns true if the slot was empty, i.e. the UI needs waking.
    fn put(&self, frame: YuvFrame) -> bool {
        self.0.lock().unwrap().replace(frame).is_none()
    }

    pub fn take(&self) -> Option<YuvFrame> {
        self.0.lock().unwrap().take()
    }
}

/// How long a frame may wait for the decoder before we give up on catching up.
const MAX_DECODE_WAIT: Duration = Duration::from_millis(300);

/// `ready` fires once the decoder exists (or failed to start), so the caller can hold off
/// reading video until then. Starting can take seconds, e.g. GStreamer's first-run plugin scan.
#[allow(clippy::too_many_arguments)]
pub fn run(
    view: usize,
    codec: Codec,
    frames: mpsc::Receiver<(VideoFrame, Instant)>,
    slot: Arc<FrameSlot>,
    proxy: EventLoopProxy<UserEvent>,
    outgoing: &UnboundedSender<ClientMsg>,
    ready: tokio::sync::oneshot::Sender<()>,
    gpu_adapter: Option<u64>,
) {
    // Arrival times of frames in the decoder, matched to pictures by timestamp.
    let arrivals: Arc<Mutex<VecDeque<(u64, Instant)>>> = Arc::default();
    let deliver: Deliver = {
        let (proxy, arrivals) = (proxy.clone(), arrivals.clone());
        Arc::new(move |mut picture: YuvFrame| {
            let mut arrivals = arrivals.lock().unwrap();
            // Anything older than this picture was dropped by the decoder.
            while arrivals.front().is_some_and(|&(pts, _)| pts < picture.pts_us) {
                arrivals.pop_front();
            }
            if arrivals.front().is_some_and(|&(pts, _)| pts == picture.pts_us) {
                picture.received_at = arrivals.pop_front().map(|(_, at)| at);
            }
            drop(arrivals);
            if slot.put(picture) {
                let _ = proxy.send_event(UserEvent::Frame(view));
            }
        })
    };
    let mut decoder = match new_decoder(codec, deliver, gpu_adapter) {
        Ok(d) => d,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Closed(view, format!("can't start {codec} decoder: {e:#}")));
            return;
        }
    };
    tracing::info!("decoding with {}", decoder.name());
    let _ = ready.send(());
    let _ = outgoing.send(ClientMsg::RequestKeyframe);
    let mut skipping = false;
    for (frame, received_at) in frames {
        // Frames that waited this long mean decoding can't keep up: skip ahead to a keyframe
        // rather than show ever older pictures. (A burst after a Wi-Fi stall doesn't wait.)
        if skipping && !frame.keyframe {
            continue;
        }
        skipping = false;
        if !frame.keyframe && received_at.elapsed() > MAX_DECODE_WAIT {
            tracing::info!(
                "decoding is {} ms behind; skipping to the next keyframe",
                received_at.elapsed().as_millis()
            );
            skipping = true;
            let _ = outgoing.send(ClientMsg::RequestKeyframe);
            continue;
        }
        {
            let mut arrivals = arrivals.lock().unwrap();
            arrivals.push_back((frame.pts_us, received_at));
            // Bound it in case a decoder never reports some frames.
            if arrivals.len() > 120 {
                arrivals.pop_front();
            }
        }
        match decoder.decode(&frame.data, frame.pts_us) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!("decode error ({e:#}), asking for a keyframe");
                let _ = outgoing.send(ClientMsg::RequestKeyframe);
            }
        }
    }
}
