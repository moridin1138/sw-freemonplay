//! Hardware H.264 and HEVC decoding with VideoToolbox.
//!
//! The stream arrives as Annex-B with parameter sets (H.264: SPS/PPS, HEVC: VPS/SPS/PPS)
//! in front of each keyframe. VideoToolbox wants a format description built from those,
//! then length-prefixed samples.

use std::ffi::c_void;
use std::ptr::{NonNull, null, null_mut};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use fmp_proto::Codec;
use objc2_core_foundation::{CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionCreateFromH264ParameterSets,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetPlaneCount, CVPixelBufferGetWidthOfPlane,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
    VTIsHardwareDecodeSupported, VTSessionSetProperty, kVTDecompressionPropertyKey_RealTime,
    kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder,
};

use super::annexb;
use super::{Decoder, Deliver, Planes, YuvFrame};

type Output = Mutex<Option<Result<YuvFrame, i32>>>;

pub fn supported_codecs() -> Vec<Codec> {
    // SAFETY: a plain capability query. (Every Apple Silicon Mac decodes HEVC in hardware.)
    if unsafe { VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC) } {
        vec![Codec::H265, Codec::H264]
    } else {
        vec![Codec::H264]
    }
}

pub struct VtDecoder {
    codec: Codec,
    /// Latest parameter sets, in `codec.parameter_set_types()` order.
    params: Vec<Vec<u8>>,
    session: Option<Session>,
    deliver: Deliver,
}

impl VtDecoder {
    pub fn new(codec: Codec, deliver: Deliver) -> Self {
        let params = vec![Vec::new(); codec.parameter_set_types().len()];
        Self { codec, params, session: None, deliver }
    }
}

impl Decoder for VtDecoder {
    fn name(&self) -> String {
        format!("VideoToolbox {} (hardware)", self.codec)
    }

    fn decode(&mut self, access_unit: &[u8], pts_us: u64) -> Result<()> {
        let mut params_changed = false;
        let mut slices = Vec::new();
        let param_types = self.codec.parameter_set_types();
        for nal in annexb::split(access_unit) {
            match param_types.iter().position(|&t| t == self.codec.nal_type(nal)) {
                Some(i) if self.params[i] != nal => {
                    self.params[i] = nal.to_vec();
                    params_changed = true;
                }
                Some(_) => {}
                None => slices.push(nal),
            }
        }
        // New parameter sets (first keyframe, or the host changed resolution) need a new session.
        if params_changed && self.params.iter().all(|p| !p.is_empty()) {
            self.session = None;
            self.session = Some(Session::new(self.codec, &self.params)?);
        }
        // Until the first keyframe brings parameter sets there is nothing we can decode.
        let Some(session) = &self.session else { return Ok(()) };
        if slices.is_empty() {
            return Ok(());
        }
        if let Some(mut picture) = session.decode(&slices)? {
            picture.pts_us = pts_us;
            (self.deliver)(picture);
        }
        Ok(())
    }
}

struct Session {
    // Field order matters: the session is invalidated in Drop before `output`, which its
    // callback writes into, is freed.
    session: CFRetained<VTDecompressionSession>,
    format: CFRetained<CMFormatDescription>,
    output: Box<Output>,
}

impl Session {
    fn new(codec: Codec, params: &[Vec<u8>]) -> Result<Self> {
        // SAFETY: standard VideoToolbox setup with valid CF objects; `output` outlives the
        // session (see field order and Drop). Parameter sets are non-empty (checked by caller).
        unsafe {
            let pointers: Vec<NonNull<u8>> = params.iter().map(|p| NonNull::from(&p[0])).collect();
            let sizes: Vec<usize> = params.iter().map(Vec::len).collect();
            let mut raw_format: *const CMFormatDescription = null();
            let (ptrs, lens, out) = (NonNull::from(&pointers[0]), NonNull::from(&sizes[0]), NonNull::from(&mut raw_format));
            let status = match codec {
                Codec::H264 => CMVideoFormatDescriptionCreateFromH264ParameterSets(None, params.len(), ptrs, lens, 4, out),
                Codec::H265 => {
                    CMVideoFormatDescriptionCreateFromHEVCParameterSets(None, params.len(), ptrs, lens, 4, None, out)
                }
            };
            if status != 0 {
                bail!("bad {codec} parameter sets from host ({status})");
            }
            let format = CFRetained::from_raw(NonNull::new(raw_format as *mut _).context("null format")?);

            let spec_keys = [kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder];
            let spec = CFDictionary::<CFString, CFBoolean>::from_slices(&spec_keys, &[CFBoolean::new(true)]);
            // NV12 in IOSurface-backed, Metal-compatible buffers, so the renderer can draw
            // the decoder's output directly as textures.
            let nv12 = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
            let no_props = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
            let attr_keys =
                [kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey];
            let attrs = CFDictionary::<CFString, CFType>::from_slices(&attr_keys, &[&nv12, &no_props, CFBoolean::new(true)]);

            let output: Box<Output> = Box::default();
            let callback = VTDecompressionOutputCallbackRecord {
                decompressionOutputCallback: Some(on_decoded),
                decompressionOutputRefCon: &*output as *const Output as *mut c_void,
            };
            let mut raw: *mut VTDecompressionSession = null_mut();
            let status = VTDecompressionSession::create(
                None,
                &format,
                Some(spec.as_opaque()),
                Some(attrs.as_opaque()),
                &callback,
                NonNull::from(&mut raw),
            );
            if status != 0 {
                bail!("VTDecompressionSessionCreate failed ({status})");
            }
            let session = CFRetained::from_raw(NonNull::new(raw).context("null session")?);
            VTSessionSetProperty(&session, kVTDecompressionPropertyKey_RealTime, Some(CFBoolean::new(true)));
            Ok(Self { session, format, output })
        }
    }

    fn decode(&self, slices: &[&[u8]]) -> Result<Option<YuvFrame>> {
        let mut avcc = Vec::with_capacity(slices.iter().map(|s| s.len() + 4).sum());
        for nal in slices {
            avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            avcc.extend_from_slice(nal);
        }
        let len = avcc.len();
        // SAFETY: CoreMedia copies `avcc` into a block it owns; the sample references our
        // format description; decoding is synchronous (no async flag) and we wait for it, so
        // the callback has filled `output` before we read it.
        unsafe {
            let mut raw_block: *mut CMBlockBuffer = null_mut();
            let status = CMBlockBuffer::create_with_memory_block(
                None,
                null_mut(),
                len,
                None,
                null(),
                0,
                len,
                kCMBlockBufferAssureMemoryNowFlag,
                NonNull::from(&mut raw_block),
            );
            if status != 0 {
                bail!("CMBlockBufferCreate failed ({status})");
            }
            let block = CFRetained::from_raw(NonNull::new(raw_block).context("null block")?);
            CMBlockBuffer::replace_data_bytes(NonNull::new_unchecked(avcc.as_mut_ptr().cast()), &block, 0, len);

            let mut raw_sample: *mut CMSampleBuffer = null_mut();
            let status = CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&self.format),
                1,
                0,
                null(),
                1,
                &len,
                NonNull::from(&mut raw_sample),
            );
            if status != 0 {
                bail!("CMSampleBufferCreate failed ({status})");
            }
            let sample = CFRetained::from_raw(NonNull::new(raw_sample).context("null sample")?);

            let status = self.session.decode_frame(&sample, VTDecodeFrameFlags(0), null_mut(), null_mut());
            if status != 0 {
                bail!("VTDecompressionSessionDecodeFrame failed ({status})");
            }
            self.session.wait_for_asynchronous_frames();
        }
        match self.output.lock().unwrap().take() {
            None => Ok(None),
            Some(Ok(frame)) => Ok(Some(frame)),
            Some(Err(status)) => bail!("hardware decode failed ({status})"),
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: after this no more callbacks run, so `output` can be freed safely.
        unsafe { self.session.invalidate() };
    }
}

unsafe extern "C-unwind" fn on_decoded(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    _flags: VTDecodeInfoFlags,
    image: *mut CVImageBuffer,
    _pts: CMTime,
    _duration: CMTime,
) {
    // SAFETY: refcon is the `Output` box owned by the live Session.
    let output = unsafe { &*(refcon as *const Output) };
    let result = if status != 0 {
        Err(status)
    } else {
        // A null image means the decoder dropped the frame.
        // SAFETY: VideoToolbox passes a valid image buffer for the duration of the call.
        match unsafe { image.as_ref() } {
            Some(img) => Ok(PixelBuffer::retain(img).into_frame()),
            None => return,
        }
    };
    *output.lock().unwrap() = Some(result);
}

/// A decoded NV12 picture still in VideoToolbox's (IOSurface-backed) buffer.
#[derive(Clone)]
pub struct PixelBuffer(pub CFRetained<CVPixelBuffer>);

// SAFETY: CVPixelBuffers are reference counted, thread-safe CF objects; the decoder is done
// writing by the time we receive one, and readers lock it or map it as a texture.
unsafe impl Send for PixelBuffer {}
unsafe impl Sync for PixelBuffer {}

impl PixelBuffer {
    fn retain(image: &CVImageBuffer) -> Self {
        // SAFETY: a valid image buffer; we take our own reference.
        Self(unsafe { CFRetained::retain(NonNull::from(image)) })
    }

    fn into_frame(self) -> YuvFrame {
        let pb = &*self.0;
        YuvFrame {
            width: CVPixelBufferGetWidthOfPlane(pb, 0) as u32,
            height: CVPixelBufferGetHeightOfPlane(pb, 0) as u32,
            planes: Planes::Native(self),
            pts_us: 0,
            received_at: None,
        }
    }

    /// Copies the planes into system memory, for when it can't be drawn directly.
    pub fn to_nv12(&self) -> Option<Planes> {
        // SAFETY: as in copy_nv12.
        unsafe { copy_nv12(&self.0) }.map(|f| f.planes)
    }
}

/// Copies an NV12 pixel buffer's planes out (with their row padding).
unsafe fn copy_nv12(pb: &CVImageBuffer) -> Option<YuvFrame> {
    if CVPixelBufferGetPlaneCount(pb) < 2 {
        return None;
    }
    // SAFETY: locked read of each plane within height * bytes_per_row, then unlock.
    unsafe {
        if CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) != 0 {
            return None;
        }
        let plane = |i: usize| {
            let base = CVPixelBufferGetBaseAddressOfPlane(pb, i) as *const u8;
            let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, i);
            let rows = CVPixelBufferGetHeightOfPlane(pb, i);
            (!base.is_null()).then(|| (std::slice::from_raw_parts(base, stride * rows).to_vec(), stride as u32))
        };
        let frame = match (plane(0), plane(1)) {
            (Some((y, y_stride)), Some((uv, uv_stride))) => Some(YuvFrame {
                width: CVPixelBufferGetWidthOfPlane(pb, 0) as u32,
                height: CVPixelBufferGetHeightOfPlane(pb, 0) as u32,
                planes: Planes::Nv12 { y, uv, y_stride, uv_stride },
                pts_us: 0,
                received_at: None,
            }),
            _ => None,
        };
        CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
        frame
    }
}
