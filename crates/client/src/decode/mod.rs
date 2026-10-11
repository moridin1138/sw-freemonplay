//! Video decoding (H.264, and HEVC where the platform can) on its own thread, handing the
//! newest picture to the renderer.
//!
//! Uses the platform's own decoder so the client ships no H.264 code of its own:
//! - macOS: VideoToolbox (hardware)
//! - Windows: Media Foundation, with DXVA hardware decoding when a D3D11 GPU is available
//! - Linux: GStreamer, preferring the VAAPI hardware decoder; bundled OpenH264 as a last resort

use std::sync::{Arc, Mutex, mpsc};

use anyhow::Result;
use fmp_proto::{ClientMsg, Codec, VideoFrame};
use tokio::sync::mpsc::UnboundedSender;
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

mod annexb;
#[cfg(target_os = "linux")]
mod gstreamer;
#[cfg(target_os = "windows")]
mod mediafoundation;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod software;
#[cfg(target_os = "macos")]
mod videotoolbox;

/// A decoded 4:2:0 picture. Planes keep the decoder's row padding (strides in bytes).
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    pub planes: Planes,
}

pub enum Planes {
    /// Separate U and V planes (the software decoder).
    #[cfg_attr(any(target_os = "macos", target_os = "windows"), allow(dead_code))]
    I420 { y: Vec<u8>, u: Vec<u8>, v: Vec<u8>, y_stride: u32, uv_stride: u32 },
    /// One interleaved CbCr plane (what hardware decoders produce).
    Nv12 { y: Vec<u8>, uv: Vec<u8>, y_stride: u32, uv_stride: u32 },
}

/// Where decoders hand finished pictures. Some decoders call it from their own threads.
pub type Deliver = Arc<dyn Fn(YuvFrame) + Send + Sync>;

pub trait Decoder {
    fn name(&self) -> String;
    /// Feeds one access unit (Annex-B). Pictures come out through `Deliver`, possibly later.
    fn decode(&mut self, access_unit: &[u8]) -> Result<()>;
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

fn new_decoder(codec: Codec, deliver: Deliver) -> Result<Box<dyn Decoder>> {
    #[cfg(target_os = "macos")]
    return Ok(Box::new(videotoolbox::VtDecoder::new(codec, deliver)));
    #[cfg(target_os = "windows")]
    return Ok(Box::new(mediafoundation::MfDecoder::new(codec, deliver)?));
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

pub fn run(
    codec: Codec,
    frames: mpsc::Receiver<VideoFrame>,
    slot: Arc<FrameSlot>,
    proxy: EventLoopProxy<UserEvent>,
    outgoing: &UnboundedSender<ClientMsg>,
) {
    let deliver: Deliver = {
        let proxy = proxy.clone();
        Arc::new(move |picture| {
            if slot.put(picture) {
                let _ = proxy.send_event(UserEvent::Frame);
            }
        })
    };
    let mut decoder = match new_decoder(codec, deliver) {
        Ok(d) => d,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Closed(format!("can't start {codec} decoder: {e:#}")));
            return;
        }
    };
    tracing::info!("decoding with {}", decoder.name());
    let _ = outgoing.send(ClientMsg::RequestKeyframe);
    for frame in frames {
        match decoder.decode(&frame.data) {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!("decode error ({e:#}), asking for a keyframe");
                let _ = outgoing.send(ClientMsg::RequestKeyframe);
            }
        }
    }
}
