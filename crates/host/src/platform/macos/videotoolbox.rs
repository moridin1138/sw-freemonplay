//! Hardware H.264 encoding with VideoToolbox, in its low-latency real-time mode.
//!
//! Captured `CVPixelBuffer`s go straight into the hardware encoder. VideoToolbox emits
//! AVCC (length-prefixed NAL units, parameter sets kept separately), so we rewrite the
//! output as Annex-B with SPS/PPS in front of every keyframe: the same format the
//! software encoder produces and the client expects.

use std::ffi::{c_int, c_void};
use std::ptr::{NonNull, null_mut};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use objc2_core_foundation::{CFArray, CFBoolean, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::{
    CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
    kCMTimeInvalid, kCMVideoCodecType_H264,
};
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPool, CVPixelBufferUnlockBaseAddress, kCVPixelBufferHeightKey,
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferPixelFormatTypeKey, kCVPixelBufferWidthKey,
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
    kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_DataRateLimits,
    kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
    kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, kVTCompressionPropertyKey_ProfileLevel,
    kVTCompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame,
    kVTProfileLevel_H264_ConstrainedHigh_AutoLevel, kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
    kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
};

use crate::encoder::{Encoded, VideoEncoder};
use crate::frame::{Frame, I420Frame};

const START_CODE: [u8; 4] = [0, 0, 0, 1];
const NAL_IDR: u8 = 5;

type Output = Mutex<Vec<Result<Encoded, i32>>>;

pub struct VtEncoder {
    // Field order matters: the session must be invalidated (in Drop) before `output`,
    // which its callback writes into, is freed.
    session: CFRetained<VTCompressionSession>,
    output: Box<Output>,
    fps: u32,
    frame_index: i64,
}

// SAFETY: a compression session may be used from any thread as long as calls aren't
// concurrent; we only ever touch it through `&mut self`.
unsafe impl Send for VtEncoder {}

pub fn hardware_encoder(width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Box<dyn VideoEncoder>> {
    Ok(Box::new(VtEncoder::new(width, height, fps, bitrate)?))
}

impl VtEncoder {
    pub fn new(width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Self> {
        let yes = CFBoolean::new(true);
        // SAFETY (all `unsafe` reads of k* statics here): framework constants, valid for
        // the life of the process.
        let spec_keys = unsafe {
            [
                kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
            ]
        };
        let spec = CFDictionary::<CFString, CFBoolean>::from_slices(&spec_keys, &[yes, yes]);
        let no_props = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
        let format = CFNumber::new_i32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32);
        let (w, h) = (CFNumber::new_i32(width as i32), CFNumber::new_i32(height as i32));
        // Describes the buffers in the session's own pool, used when frames arrive as I420.
        let attr_keys = unsafe {
            [
                kCVPixelBufferPixelFormatTypeKey,
                kCVPixelBufferWidthKey,
                kCVPixelBufferHeightKey,
                kCVPixelBufferIOSurfacePropertiesKey,
            ]
        };
        let attrs = CFDictionary::<CFString, CFType>::from_slices(&attr_keys, &[&format, &w, &h, &no_props]);

        let output: Box<Output> = Box::default();
        let mut raw: *mut VTCompressionSession = null_mut();
        // SAFETY: arguments are valid CF objects; `output` outlives the session (see field order).
        let status = unsafe {
            VTCompressionSession::create(
                None,
                width as i32,
                height as i32,
                kCMVideoCodecType_H264,
                Some(spec.as_opaque()),
                Some(attrs.as_opaque()),
                None,
                Some(on_encoded),
                &*output as *const Output as *mut c_void,
                NonNull::from(&mut raw),
            )
        };
        if status != 0 {
            bail!("VTCompressionSessionCreate failed ({status}){}", hint(status));
        }
        // SAFETY: Create returned +1 ownership of a non-null session.
        let session = unsafe { CFRetained::from_raw(NonNull::new(raw).context("null session")?) };

        let set = |key: &CFString, value: &CFType, required: bool| -> Result<()> {
            // SAFETY: setting a documented property on our session.
            let status = unsafe { VTSessionSetProperty(&session, key, Some(value)) };
            match (status, required) {
                (0, _) => Ok(()),
                (s, true) => bail!("setting {key} failed ({s})"),
                (s, false) => {
                    tracing::debug!("optional encoder property {key} not supported ({s})");
                    Ok(())
                }
            }
        };
        let bytes_per_sec = bitrate as f64 / 8.0;
        // Allow short bursts (a keyframe after a big screen change) up to 2x the average.
        let limits = CFArray::<CFNumber>::from_retained_objects(&[
            CFNumber::new_f64(bytes_per_sec * 2.0),
            CFNumber::new_f64(1.0),
        ]);
        // SAFETY: framework constants, valid for the life of the process.
        unsafe {
            set(kVTCompressionPropertyKey_RealTime, yes, true)?;
            set(kVTCompressionPropertyKey_AllowFrameReordering, CFBoolean::new(false), true)?;
            set(kVTCompressionPropertyKey_ProfileLevel, kVTProfileLevel_H264_ConstrainedHigh_AutoLevel, true)?;
            set(kVTCompressionPropertyKey_AverageBitRate, &CFNumber::new_i32(bitrate as i32), true)?;
            set(kVTCompressionPropertyKey_DataRateLimits, &limits, false)?;
            set(kVTCompressionPropertyKey_ExpectedFrameRate, &CFNumber::new_i32(fps as i32), false)?;
            // Periodic IDR so a client recovers on its own even if a keyframe request is lost.
            set(kVTCompressionPropertyKey_MaxKeyFrameInterval, &CFNumber::new_i32((fps * 10) as i32), false)?;
            set(kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, yes, false)?;
        }

        Ok(Self { session, output, fps: fps.max(1), frame_index: 0 })
    }

    /// Copies an I420 frame into a pixel buffer from the session's pool (test pattern path).
    fn upload(&self, frame: &I420Frame) -> Result<CFRetained<CVPixelBuffer>> {
        // SAFETY: standard pool allocation, then a locked copy within each plane's bounds.
        unsafe {
            let pool = self.session.pixel_buffer_pool().context("encoder has no pixel buffer pool")?;
            let mut raw: *mut CVPixelBuffer = null_mut();
            let status = CVPixelBufferPool::create_pixel_buffer(None, &pool, NonNull::from(&mut raw));
            if status != 0 {
                bail!("CVPixelBufferPoolCreatePixelBuffer failed ({status})");
            }
            let pb = CFRetained::from_raw(NonNull::new(raw).context("null pixel buffer")?);
            CVPixelBufferLockBaseAddress(&pb, CVPixelBufferLockFlags::empty());
            let y = CVPixelBufferGetBaseAddressOfPlane(&pb, 0) as *mut u8;
            let ys = CVPixelBufferGetBytesPerRowOfPlane(&pb, 0);
            for row in 0..frame.height {
                let src = &frame.y[row * frame.width..][..frame.width];
                std::ptr::copy_nonoverlapping(src.as_ptr(), y.add(row * ys), frame.width);
            }
            let uv = CVPixelBufferGetBaseAddressOfPlane(&pb, 1) as *mut u8;
            let uvs = CVPixelBufferGetBytesPerRowOfPlane(&pb, 1);
            let cw = frame.width / 2;
            for row in 0..frame.height / 2 {
                let dst = uv.add(row * uvs);
                for col in 0..cw {
                    *dst.add(col * 2) = frame.u[row * cw + col];
                    *dst.add(col * 2 + 1) = frame.v[row * cw + col];
                }
            }
            CVPixelBufferUnlockBaseAddress(&pb, CVPixelBufferLockFlags::empty());
            Ok(pb)
        }
    }
}

impl VideoEncoder for VtEncoder {
    fn name(&self) -> &'static str {
        "VideoToolbox (hardware)"
    }

    fn encode(&mut self, frame: &Frame, force_keyframe: bool) -> Result<Option<Encoded>> {
        let buffer = match frame {
            Frame::Native(f) => f.buffer.clone(),
            Frame::I420(f) => self.upload(f)?,
        };
        let force = force_keyframe.then(|| {
            // SAFETY: framework constant.
            let keys = unsafe { [kVTEncodeFrameOptionKey_ForceKeyFrame] };
            CFDictionary::<CFString, CFBoolean>::from_slices(&keys, &[CFBoolean::new(true)])
        });
        // SAFETY: encoding a valid pixel buffer on our session. CompleteFrames blocks until
        // the callback has run for everything submitted, so `output` is filled on return.
        unsafe {
            let pts = CMTime::new(self.frame_index, self.fps as i32);
            self.frame_index += 1;
            let status = self.session.encode_frame(
                &buffer,
                pts,
                kCMTimeInvalid,
                force.as_deref().map(|d| d.as_opaque()),
                null_mut(),
                null_mut(),
            );
            if status != 0 {
                bail!("VTCompressionSessionEncodeFrame failed ({status})");
            }
            self.session.complete_frames(kCMTimeInvalid);
        }
        let mut results = std::mem::take(&mut *self.output.lock().unwrap());
        match results.pop() {
            None => Ok(None),
            Some(Ok(encoded)) => Ok(Some(encoded)),
            Some(Err(status)) => bail!("hardware encode failed ({status})"),
        }
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        // SAFETY: after this no more callbacks run, so `output` can be freed safely.
        unsafe { self.session.invalidate() };
    }
}

unsafe extern "C-unwind" fn on_encoded(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: i32,
    _flags: VTEncodeInfoFlags,
    sample: *mut CMSampleBuffer,
) {
    // SAFETY: refcon is the `Output` box owned by the live VtEncoder.
    let output = unsafe { &*(refcon as *const Output) };
    let result = if status != 0 {
        Err(status)
    } else {
        // A null sample means the encoder dropped the frame.
        // SAFETY: VideoToolbox passes a valid sample buffer for the duration of the call.
        match unsafe { sample.as_ref() }.and_then(|s| unsafe { to_annex_b(s) }) {
            Some(encoded) => Ok(encoded),
            None => return,
        }
    };
    output.lock().unwrap().push(result);
}

/// Rewrites one AVCC sample as Annex-B, putting SPS/PPS in front of IDR frames.
unsafe fn to_annex_b(sample: &CMSampleBuffer) -> Option<Encoded> {
    // SAFETY: reading the sample's own data and format description.
    unsafe {
        let block = sample.data_buffer()?;
        let len = block.data_length();
        let mut avcc = vec![0u8; len];
        if block.copy_data_bytes(0, len, NonNull::new(avcc.as_mut_ptr().cast())?) != 0 {
            return None;
        }
        let desc = sample.format_description()?;
        let (param_count, nal_len_size) = parameter_set_info(&desc)?;

        let nals = split_avcc(&avcc, nal_len_size)?;
        let keyframe = nals.iter().any(|n| n.first().is_some_and(|b| b & 0x1F == NAL_IDR));
        let mut data = Vec::with_capacity(len + 64);
        if keyframe {
            for i in 0..param_count {
                let mut ptr: *const u8 = std::ptr::null();
                let mut size = 0usize;
                let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    &desc,
                    i,
                    &mut ptr,
                    &mut size,
                    null_mut(),
                    null_mut(),
                );
                if status == 0 && !ptr.is_null() {
                    data.extend_from_slice(&START_CODE);
                    data.extend_from_slice(std::slice::from_raw_parts(ptr, size));
                }
            }
        }
        for nal in nals {
            data.extend_from_slice(&START_CODE);
            data.extend_from_slice(nal);
        }
        Some(Encoded { data, keyframe })
    }
}

/// (number of parameter sets, NAL length prefix size)
unsafe fn parameter_set_info(desc: &CMFormatDescription) -> Option<(usize, usize)> {
    let mut count = 0usize;
    let mut header_len: c_int = 0;
    // SAFETY: querying counts only; null for the outputs we don't need.
    let status = unsafe {
        CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            desc,
            0,
            null_mut(),
            null_mut(),
            &mut count,
            &mut header_len,
        )
    };
    (status == 0 && (1..=4).contains(&header_len)).then_some((count, header_len as usize))
}

/// Splits length-prefixed NAL units. `None` if the data is malformed.
fn split_avcc(mut data: &[u8], len_size: usize) -> Option<Vec<&[u8]>> {
    let mut nals = Vec::new();
    while !data.is_empty() {
        let (len_bytes, rest) = data.split_at_checked(len_size)?;
        let len = len_bytes.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize);
        let (nal, rest) = rest.split_at_checked(len)?;
        nals.push(nal);
        data = rest;
    }
    Some(nals)
}

fn hint(status: i32) -> &'static str {
    match status {
        -12908 => " - no hardware encoder for this size/format",
        -12902 => " - bad parameter (resolution too large?)",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_avcc() {
        let data = [0, 0, 0, 2, 0x65, 0xAA, 0, 0, 0, 1, 0x41];
        let nals = split_avcc(&data, 4).unwrap();
        assert_eq!(nals, vec![&[0x65, 0xAA][..], &[0x41][..]]);
        assert!(split_avcc(&[0, 0, 0, 9, 1], 4).is_none());
    }

    #[test]
    fn hardware_encodes_test_frames() {
        let (w, h) = (640usize, 480usize);
        let mut enc = VtEncoder::new(w as u32, h as u32, 30, 2_000_000).expect("VideoToolbox session");
        let frame = |shade: u8| {
            Frame::I420(I420Frame {
                width: w,
                height: h,
                y: vec![shade; w * h],
                u: vec![128; w * h / 4],
                v: vec![128; w * h / 4],
                captured_at: std::time::Instant::now(),
            })
        };
        let first = enc.encode(&frame(16), true).unwrap().expect("first frame");
        assert!(first.keyframe, "forced keyframe");
        assert_eq!(&first.data[..4], &START_CODE);
        assert_eq!(first.data[4] & 0x1F, 7, "keyframe starts with SPS");
        let second = enc.encode(&frame(200), false).unwrap().expect("second frame");
        assert!(!second.keyframe);
    }
}
