//! Video encoding. Each platform can offer a hardware encoder (VideoToolbox on macOS so
//! far, H.264 and HEVC); the bundled OpenH264 software encoder (H.264 only) works
//! everywhere as the fallback.

use anyhow::{Context, Result, bail};
use clap::ValueEnum;
use fmp_proto::Codec;
use openh264::OpenH264API;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode,
    UsageType,
};

use crate::frame::Frame;

pub struct Encoded {
    /// Annex-B access unit. Keyframes carry their parameter sets.
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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CodecChoice {
    /// Whatever the client prefers that this host can encode.
    Auto,
    H264,
    H265,
}

impl CodecChoice {
    pub fn only(self) -> Option<Codec> {
        match self {
            CodecChoice::Auto => None,
            CodecChoice::H264 => Some(Codec::H264),
            CodecChoice::H265 => Some(Codec::H265),
        }
    }
}

/// Picks the first codec in the client's preference order that we can encode (optionally
/// restricted to `only`), and creates its encoder. Returns the encoder, codec and bitrate.
pub fn negotiate(
    choice: EncoderChoice,
    only: Option<Codec>,
    client_codecs: &[Codec],
    (width, height, fps): (u32, u32, u32),
    bitrate_for: impl Fn(Codec) -> u32,
) -> Result<(Box<dyn VideoEncoder>, Codec, u32)> {
    let mut problems = Vec::new();
    for &codec in client_codecs.iter().filter(|&&c| only.is_none_or(|o| o == c)) {
        let bitrate = bitrate_for(codec);
        match create(choice, codec, width, height, fps, bitrate) {
            Ok(encoder) => return Ok((encoder, codec, bitrate)),
            Err(e) => problems.push(format!("{codec}: {e:#}")),
        }
    }
    let offered: Vec<String> = client_codecs.iter().map(Codec::to_string).collect();
    let mut message = format!("no usable codec in common (client decodes {})", offered.join(", "));
    if let Some(o) = only {
        message += &format!("; host is limited to {o}");
    }
    if !problems.is_empty() {
        message += &format!("; {}", problems.join("; "));
    }
    bail!(message)
}

pub fn create(
    choice: EncoderChoice,
    codec: Codec,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
) -> Result<Box<dyn VideoEncoder>> {
    let software = || -> Result<Box<dyn VideoEncoder>> {
        if codec != Codec::H264 {
            bail!("the software encoder only does H.264");
        }
        Ok(Box::new(SoftwareEncoder::new(fps, bitrate)?))
    };
    let hardware = || {
        crate::platform::hardware_encoder(codec, width, height, fps, bitrate).context("hardware encoder unavailable")
    };
    match choice {
        EncoderChoice::Software => software(),
        EncoderChoice::Hardware => hardware(),
        EncoderChoice::Auto => hardware().or_else(|e| {
            if codec == Codec::H264 {
                tracing::warn!("{e:#}; using software");
            }
            software()
        }),
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

/// A reasonable LAN bitrate for screen content: about 0.1 bits per pixel per frame for
/// H.264 (HEVC gets 60% of that for similar quality), clamped to 3..=50 Mbit/s.
pub fn default_bitrate(codec: Codec, width: u32, height: u32, fps: u32) -> u32 {
    let bits_per_pixel = match codec {
        Codec::H264 => 0.1,
        Codec::H265 => 0.06,
    };
    let bps = (width as f64 * height as f64 * fps as f64 * bits_per_pixel) as u64;
    bps.clamp(3_000_000, 50_000_000) as u32
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
