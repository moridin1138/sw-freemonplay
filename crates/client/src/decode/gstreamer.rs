//! H.264 / HEVC decoding through the system's GStreamer.
//!
//! We pick the best decoder installed: VAAPI on Intel/AMD GPUs (`vah264dec`/`vah265dec`
//! from gst-plugins-bad, or the older `vaapih26xdec`), otherwise a software decoder the
//! distro ships (`avdec_h264`/`avdec_h265` from gst-libav, `openh264dec`). Either way the
//! codec implementation comes from the system, not from us.
//!
//! On Debian/Ubuntu: `sudo apt install gstreamer1.0-plugins-bad gstreamer1.0-libav intel-media-va-driver`

use anyhow::{Context, Result, anyhow, bail};
use fmp_proto::Codec;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::{AppSink, AppSinkCallbacks, AppSrc};
use gstreamer_video::prelude::*;
use gstreamer_video::{VideoFormat, VideoFrameRef, VideoInfo};

use super::{Decoder, Deliver, Planes, YuvFrame};

/// Tried in order: VAAPI hardware first, then software decoders distros commonly ship.
/// Measured at 60 fps, the pipeline itself holds back no frames with avdec_h264.
const H264_DECODERS: &[&str] = &["vah264dec", "vaapih264dec", "avdec_h264", "openh264dec"];
const HEVC_DECODERS: &[&str] = &["vah265dec", "vaapih265dec", "avdec_h265"];

fn has_element(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

pub fn supported_codecs() -> Vec<Codec> {
    // HEVC only when a VAAPI decoder can do it on the GPU. (H.264 always works: if
    // GStreamer can't, we fall back to bundled OpenH264.)
    if gst::init().is_ok() && (has_element("vah265dec") || has_element("vaapih265dec")) {
        vec![Codec::H265, Codec::H264]
    } else {
        vec![Codec::H264]
    }
}

pub struct GstDecoder {
    pipeline: gst::Pipeline,
    src: AppSrc,
    codec: Codec,
    element: &'static str,
}

impl GstDecoder {
    pub fn new(codec: Codec, deliver: Deliver) -> Result<Self> {
        gst::init()?;
        let (candidates, caps, parser) = match codec {
            Codec::H264 => (H264_DECODERS, "video/x-h264", "h264parse"),
            Codec::H265 => (HEVC_DECODERS, "video/x-h265", "h265parse"),
        };
        // FMP_GST_DECODER=avdec_h264 (say) forces a particular element, for troubleshooting.
        let forced = std::env::var("FMP_GST_DECODER").ok().map(|s| &*s.leak());
        let element = forced.or_else(|| candidates.iter().copied().find(|name| has_element(name))).with_context(|| {
            format!("GStreamer has no {codec} decoder; install gstreamer1.0-plugins-bad or gstreamer1.0-libav")
        })?;

        // sync=false: show frames as soon as they're decoded, never wait on a clock.
        // max-buffers/drop: if the UI falls behind, keep only the newest pictures.
        let description = format!(
            "appsrc name=src is-live=true format=time \
                caps={caps},stream-format=byte-stream,alignment=au \
             ! {parser} ! {element} \
             ! appsink name=sink sync=false max-buffers=2 drop=true \
                caps=video/x-raw,format=(string){{NV12,I420}}"
        );
        let pipeline = gst::parse::launch(&description)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<AppSrc>().ok())
            .context("appsrc missing")?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<AppSink>().ok())
            .context("appsink missing")?;

        sink.set_callbacks(
            AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    if let Some(picture) = to_yuv(&sample) {
                        deliver(picture);
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing).context("GStreamer pipeline wouldn't start")?;
        Ok(Self { pipeline, src, codec, element })
    }

    /// Returns the first error the pipeline has reported since the last call, if any.
    fn take_error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        while let Some(msg) = bus.pop_filtered(&[gst::MessageType::Error]) {
            if let gst::MessageView::Error(err) = msg.view() {
                return Some(err.error().to_string());
            }
        }
        None
    }

    /// After an error GStreamer stops the pipeline; restart it so the next keyframe recovers.
    fn restart(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
        let _ = self.pipeline.set_state(gst::State::Playing);
    }
}

impl Decoder for GstDecoder {
    fn name(&self) -> String {
        let kind = if self.element.starts_with("va") { "hardware" } else { "software" };
        format!("GStreamer {} {} ({kind})", self.codec, self.element)
    }

    fn decode(&mut self, access_unit: &[u8], pts_us: u64) -> Result<()> {
        if let Some(error) = self.take_error() {
            self.restart();
            bail!("GStreamer: {error}");
        }
        let mut buffer = gst::Buffer::from_slice(access_unit.to_vec());
        // The host's timestamps; GStreamer carries them through to the decoded sample.
        buffer.get_mut().context("fresh buffer is writable")?.set_pts(gst::ClockTime::from_useconds(pts_us));
        self.src.push_buffer(buffer).map_err(|e| anyhow!("pushing to GStreamer: {e:?}"))?;
        Ok(())
    }
}

impl Drop for GstDecoder {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

fn to_yuv(sample: &gst::Sample) -> Option<YuvFrame> {
    let info = VideoInfo::from_caps(sample.caps()?).ok()?;
    let frame = VideoFrameRef::from_buffer_ref_readable(sample.buffer()?, &info).ok()?;
    let stride = |i: usize| frame.plane_stride()[i] as u32;
    let plane = |i: u32| frame.plane_data(i).ok().map(<[u8]>::to_vec);
    let planes = match info.format() {
        VideoFormat::Nv12 => Planes::Nv12 { y: plane(0)?, uv: plane(1)?, y_stride: stride(0), uv_stride: stride(1) },
        VideoFormat::I420 => Planes::I420 {
            y: plane(0)?,
            u: plane(1)?,
            v: plane(2)?,
            y_stride: stride(0),
            uv_stride: stride(1),
        },
        _ => return None,
    };
    let pts_us = sample.buffer()?.pts().map_or(0, |t| t.useconds());
    Some(YuvFrame { width: info.width(), height: info.height(), planes, pts_us, received_at: None })
}
