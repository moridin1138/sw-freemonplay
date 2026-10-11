//! Display capture with ScreenCaptureKit. Frames stay as the native NV12 `CVPixelBuffer`
//! (an IOSurface), which VideoToolbox can encode without any copy.
//!
//! Uses the raw objc2 framework bindings (no Swift bridge), so it builds with just the
//! Xcode Command Line Tools.

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBuffer,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeightOfPlane,
    CVPixelBufferGetPlaneCount, CVPixelBufferGetWidthOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};

use super::VirtualDisplay;
use crate::frame::{Frame, FrameSlot, I420Frame};

/// A captured frame still in its native buffer.
pub struct NativeFrame {
    pub buffer: CFRetained<CVPixelBuffer>,
    pub captured_at: Instant,
}

// SAFETY: CVPixelBuffers are reference counted, thread-safe CF objects. We never write to
// one after capture, and reads lock it first.
unsafe impl Send for NativeFrame {}
unsafe impl Sync for NativeFrame {}

impl NativeFrame {
    /// Copies the NV12 picture out as packed I420 (for the software encoder).
    pub fn to_i420(&self) -> Option<I420Frame> {
        let pb = &*self.buffer;
        if CVPixelBufferGetPlaneCount(pb) < 2 {
            return None;
        }
        // SAFETY: lock for reading, copy out, unlock. Plane pointers are only used while
        // locked, and each plane holds height * bytes_per_row bytes.
        unsafe {
            if CVPixelBufferLockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly) != 0 {
                return None;
            }
            let plane = |i: usize| {
                let base = CVPixelBufferGetBaseAddressOfPlane(pb, i) as *const u8;
                let stride = CVPixelBufferGetBytesPerRowOfPlane(pb, i);
                let rows = CVPixelBufferGetHeightOfPlane(pb, i);
                (!base.is_null()).then(|| (std::slice::from_raw_parts(base, stride * rows), stride))
            };
            let width = CVPixelBufferGetWidthOfPlane(pb, 0) & !1;
            let height = CVPixelBufferGetHeightOfPlane(pb, 0) & !1;
            let frame = match (plane(0), plane(1)) {
                (Some((y, ys)), Some((uv, uvs))) => {
                    let mut f = I420Frame::from_nv12(width, height, y, ys, uv, uvs);
                    f.captured_at = self.captured_at;
                    Some(f)
                }
                _ => None,
            };
            CVPixelBufferUnlockBaseAddress(pb, CVPixelBufferLockFlags::ReadOnly);
            frame
        }
    }
}

pub struct Capture {
    stream: Retained<SCStream>,
    _output: Retained<StreamOutput>,
}

// SAFETY: SCStream is documented as usable from any thread; we only start/stop it.
unsafe impl Send for Capture {}

impl Capture {
    /// `width`/`height` is the stream size; ScreenCaptureKit scales the display to it.
    pub fn start(display: &VirtualDisplay, width: u32, height: u32, fps: u32, slot: Arc<FrameSlot>) -> Result<Self> {
        let sc_display = find_display(display.id())?;
        // SAFETY: standard ScreenCaptureKit setup; every object is retained for as long as it's used.
        unsafe {
            let filter = SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &sc_display,
                &NSArray::new(),
            );
            let config = SCStreamConfiguration::new();
            config.setWidth(width as usize);
            config.setHeight(height as usize);
            config.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
            config.setMinimumFrameInterval(CMTime::new(1, fps.max(1) as i32));
            config.setShowsCursor(true);
            // We hold up to two buffers (newest + the one being encoded); leave the
            // capturer spares so it never stalls waiting for us.
            config.setQueueDepth(5);

            let stream = SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &config, None);
            let output = StreamOutput::new(slot);
            let queue = DispatchQueue::new("freemonplay.capture", None);
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    Some(&queue),
                )
                .map_err(|e| anyhow!("addStreamOutput: {}", e.localizedDescription()))?;

            let (tx, rx) = mpsc::channel();
            let done = RcBlock::new(move |err: *mut NSError| {
                let _ = tx.send(err.as_ref().map(|e| e.localizedDescription().to_string()));
            });
            stream.startCaptureWithCompletionHandler(Some(&done));
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(None) => {}
                Ok(Some(e)) => bail!("ScreenCaptureKit failed to start: {e}"),
                Err(_) => bail!("ScreenCaptureKit didn't start within 5 s"),
            }
            Ok(Self { stream, _output: output })
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        // SAFETY: stopping a stream we started; no completion handler needed.
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "FreemonplayStreamOutput"]
    #[ivars = Arc<FrameSlot>]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(&self, _stream: &SCStream, sample: &CMSampleBuffer, kind: SCStreamOutputType) {
            // Idle frames (nothing changed on screen) carry no image; the client keeps the last one.
            // SAFETY: reading a sample buffer ScreenCaptureKit handed us.
            if kind == SCStreamOutputType::Screen
                && let Some(buffer) = unsafe { sample.image_buffer() }
            {
                self.ivars().push(Frame::Native(NativeFrame { buffer, captured_at: Instant::now() }));
            }
        }
    }
);

impl StreamOutput {
    fn new(slot: Arc<FrameSlot>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(slot);
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

/// A freshly created virtual display takes a moment to show up in the shareable list.
fn find_display(id: u32) -> Result<Retained<SCDisplay>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
            // SAFETY: ScreenCaptureKit passes valid (or null) objects for the duration of the call.
            let result = unsafe {
                match (content.as_ref(), err.as_ref()) {
                    (Some(c), _) => Ok(c.displays().iter().find(|d| d.displayID() == id)),
                    (None, Some(e)) => Err(e.localizedDescription().to_string()),
                    (None, None) => Err("no shareable content".to_string()),
                }
            };
            let _ = tx.send(result);
        });
        // SAFETY: the block lives until it has been called.
        unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };
        let found = rx
            .recv_timeout(Duration::from_secs(5))
            .context("ScreenCaptureKit didn't answer")?
            .map_err(|e| anyhow!("ScreenCaptureKit refused ({e}); check Screen Recording permission"))?;
        if let Some(d) = found {
            return Ok(d);
        }
        if Instant::now() > deadline {
            bail!("virtual display {id} never appeared in ScreenCaptureKit");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
