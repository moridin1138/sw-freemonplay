//! Raw frames and the hand-off between the capture callback and the encoder thread.

use std::borrow::Cow;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A captured picture. Hardware encoders take the OS's native buffer directly (no copy);
/// the software encoder converts to I420 when it needs to.
pub enum Frame {
    I420(I420Frame),
    #[cfg(target_os = "macos")]
    Native(crate::platform::NativeFrame),
}

impl Frame {
    pub fn captured_at(&self) -> Instant {
        match self {
            Frame::I420(f) => f.captured_at,
            #[cfg(target_os = "macos")]
            Frame::Native(f) => f.captured_at,
        }
    }

    pub fn to_i420(&self) -> Option<Cow<'_, I420Frame>> {
        match self {
            Frame::I420(f) => Some(Cow::Borrowed(f)),
            #[cfg(target_os = "macos")]
            Frame::Native(f) => f.to_i420().map(Cow::Owned),
        }
    }
}

/// A tightly packed I420 frame (no row padding).
#[derive(Clone)]
pub struct I420Frame {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub captured_at: Instant,
}

impl I420Frame {
    /// Converts an NV12 frame (Y plane plus interleaved CbCr plane, as delivered by most
    /// capture APIs) into packed I420, dropping any row padding.
    pub fn from_nv12(width: usize, height: usize, y: &[u8], y_stride: usize, uv: &[u8], uv_stride: usize) -> Self {
        let (cw, ch) = (width / 2, height / 2);
        let mut out_y = Vec::with_capacity(width * height);
        for row in y.chunks(y_stride).take(height) {
            out_y.extend_from_slice(&row[..width]);
        }
        let mut out_u = Vec::with_capacity(cw * ch);
        let mut out_v = Vec::with_capacity(cw * ch);
        for row in uv.chunks(uv_stride).take(ch) {
            for [u, v] in row[..cw * 2].as_chunks::<2>().0 {
                out_u.push(*u);
                out_v.push(*v);
            }
        }
        Self { width, height, y: out_y, u: out_u, v: out_v, captured_at: Instant::now() }
    }
}

impl openh264::formats::YUVSource for I420Frame {
    fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.width, self.width / 2, self.width / 2)
    }
    fn y(&self) -> &[u8] {
        &self.y
    }
    fn u(&self) -> &[u8] {
        &self.u
    }
    fn v(&self) -> &[u8] {
        &self.v
    }
}

/// Holds only the newest frame. If the encoder or network falls behind, older frames are
/// simply overwritten, which keeps latency bounded instead of building a queue.
#[derive(Default)]
pub struct FrameSlot {
    state: Mutex<SlotState>,
    cv: Condvar,
}

#[derive(Default)]
struct SlotState {
    latest: Option<Arc<Frame>>,
    fresh: bool,
    want_keyframe: bool,
    closed: bool,
}

pub enum Next {
    Frame { frame: Arc<Frame>, keyframe: bool },
    Closed,
}

impl FrameSlot {
    pub fn push(&self, frame: Frame) {
        let mut s = self.state.lock().unwrap();
        s.latest = Some(Arc::new(frame));
        s.fresh = true;
        self.cv.notify_one();
    }

    /// Ask for an IDR frame. Re-sends the last frame if the screen is idle and nothing new arrives.
    pub fn request_keyframe(&self) {
        let mut s = self.state.lock().unwrap();
        s.want_keyframe = true;
        self.cv.notify_one();
    }

    /// Whether capture has delivered at least one frame.
    pub fn has_frame(&self) -> bool {
        self.state.lock().unwrap().latest.is_some()
    }

    pub fn close(&self) {
        self.state.lock().unwrap().closed = true;
        self.cv.notify_all();
    }

    /// Blocks until there is a new frame, a keyframe request with a frame to encode, or close.
    pub fn next(&self) -> Next {
        let mut s = self.state.lock().unwrap();
        loop {
            if s.closed {
                return Next::Closed;
            }
            let ready = s.fresh || (s.want_keyframe && s.latest.is_some());
            if ready {
                s.fresh = false;
                let keyframe = std::mem::take(&mut s.want_keyframe);
                return Next::Frame { frame: s.latest.clone().unwrap(), keyframe };
            }
            s = self.cv.wait_timeout(s, Duration::from_millis(500)).unwrap().0;
        }
    }
}
