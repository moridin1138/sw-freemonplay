//! H.264 encoding. Each platform can offer a hardware encoder (VideoToolbox on macOS so
//! far); the bundled OpenH264 software encoder works everywhere as the fallback.

use anyhow::{Context, Result};
use clap::ValueEnum;
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode,
    UsageType,
};

use crate::frame::Frame;

pub struct Encoded {
    /// H.264 Annex-B access unit. Keyframes carry their SPS/PPS.
    pub data: Vec<u8>,
    pub keyframe: bool,
}

pub trait VideoEncoder: Send {
    fn name(&self) -> &'static str;
    /// Returns `None` if the encoder chose to drop this frame.
    fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Option<Encoded>>;
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum EncoderChoice {
    /// Hardware if this machine has it, otherwise software.
    Auto,
    Hardware,
    Software,
}

pub fn create(choice: EncoderChoice, width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Box<dyn VideoEncoder>> {
    match choice {
        EncoderChoice::Software => Ok(Box::new(SoftwareEncoder::new(fps, bitrate)?)),
        EncoderChoice::Hardware => {
            crate::platform::hardware_encoder(width, height, fps, bitrate).context("hardware encoder unavailable")
        }
        EncoderChoice::Auto => match crate::platform::hardware_encoder(width, height, fps, bitrate) {
            Ok(e) => Ok(e),
            Err(e) => {
                tracing::warn!("no hardware encoder ({e:#}); using software");
                Ok(Box::new(SoftwareEncoder::new(fps, bitrate)?))
            }
        },
    }
}

/// OpenH264, compiled from source. Portable but CPU heavy above 1440p.
pub struct SoftwareEncoder {
    enc: Encoder,
}

impl SoftwareEncoder {
    pub fn new(fps: u32, bitrate_bps: u32) -> Result<Self> {
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            // We already drop frames upstream when behind; letting the encoder skip too
            // makes the picture stutter on fast changes.
            .skip_frames(false)
            // Periodic IDR so a client recovers on its own even if a keyframe request is lost.
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 10))
            .complexity(Complexity::Low)
            // Neither is supported for screen content; saying so avoids startup warnings.
            .adaptive_quantization(false)
            .background_detection(false);
        Ok(Self { enc: Encoder::with_api_config(OpenH264API::from_source(), config)? })
    }

}

impl VideoEncoder for SoftwareEncoder {
    fn name(&self) -> &'static str {
        "OpenH264 (software)"
    }

    fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Option<Encoded>> {
        let Some(frame) = frame.to_i420() else { return Ok(None) };
        if force_keyframe {
            self.enc.force_intra_frame();
        }
        let bitstream = self.enc.encode(&*frame)?;
        let frame_type = bitstream.frame_type();
        if matches!(frame_type, FrameType::Skip | FrameType::Invalid) {
            return Ok(None);
        }
        let data = bitstream.to_vec();
        if data.is_empty() {
            return Ok(None);
        }
        Ok(Some(Encoded { data, keyframe: matches!(frame_type, FrameType::IDR | FrameType::I) }))
    }
}

/// A reasonable LAN bitrate for screen content: about 0.1 bits per pixel per frame,
/// clamped to 4..=50 Mbit/s.
pub fn default_bitrate(width: u32, height: u32, fps: u32) -> u32 {
    let bps = (width as f64 * height as f64 * fps as f64 * 0.1) as u64;
    bps.clamp(4_000_000, 50_000_000) as u32
}

/// OpenH264 tops out at 3840x2160 and needs even dimensions. Scale down to fit, keeping
/// the aspect ratio.
pub fn fit_stream_size(width: u32, height: u32) -> (u32, u32) {
    const MAX_W: f64 = 3840.0;
    const MAX_H: f64 = 2160.0;
    let (w, h) = (width as f64, height as f64);
    let (max_w, max_h) = if w >= h { (MAX_W, MAX_H) } else { (MAX_H, MAX_W) };
    let scale = (max_w / w).min(max_h / h).min(1.0);
    let even = |v: f64| ((v * scale) as u32 / 2 * 2).max(2);
    (even(w), even(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_5k_into_4k() {
        assert_eq!(fit_stream_size(5120, 2880), (3840, 2160));
        assert_eq!(fit_stream_size(1920, 1080), (1920, 1080));
        assert_eq!(fit_stream_size(2732, 2048), (2732, 2048));
        assert_eq!(fit_stream_size(1179, 2556), (1178, 2556));
    }

    #[test]
    fn encodes_a_frame() {
        let (w, h) = (64, 48);
        let frame = crate::frame::I420Frame {
            width: w,
            height: h,
            y: vec![128; w * h],
            u: vec![128; w * h / 4],
            v: vec![128; w * h / 4],
            captured_at: std::time::Instant::now(),
        };
        let mut enc = SoftwareEncoder::new(30, 1_000_000).unwrap();
        let out = enc.encode(&Frame::I420(frame), true).unwrap().expect("first frame is encoded");
        assert!(out.keyframe);
        assert_eq!(&out.data[..4], &[0, 0, 0, 1]);
    }
}
