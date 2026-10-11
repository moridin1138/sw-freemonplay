//! H.264 decoding on its own thread, handing the newest picture to the renderer.

use std::sync::{Mutex, mpsc};

use fmp_proto::{ClientMsg, VideoFrame};
use openh264::decoder::Decoder;
use openh264::formats::YUVSource;
use tokio::sync::mpsc::UnboundedSender;
use winit::event_loop::EventLoopProxy;

use crate::UserEvent;

/// A decoded I420 picture. Planes keep the decoder's row padding (`strides`).
pub struct YuvFrame {
    pub width: u32,
    pub height: u32,
    pub strides: (u32, u32),
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
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
    frames: mpsc::Receiver<VideoFrame>,
    slot: &FrameSlot,
    proxy: &EventLoopProxy<UserEvent>,
    outgoing: &UnboundedSender<ClientMsg>,
) {
    let mut decoder = match Decoder::new() {
        Ok(d) => d,
        Err(e) => {
            let _ = proxy.send_event(UserEvent::Closed(format!("can't start H.264 decoder: {e}")));
            return;
        }
    };
    let _ = outgoing.send(ClientMsg::RequestKeyframe);
    for frame in frames {
        match decoder.decode(&frame.data) {
            Ok(Some(yuv)) => {
                let (w, h) = yuv.dimensions();
                let (sy, su, _) = yuv.strides();
                let picture = YuvFrame {
                    width: w as u32,
                    height: h as u32,
                    strides: (sy as u32, su as u32),
                    y: yuv.y()[..sy * h].to_vec(),
                    u: yuv.u()[..su * h.div_ceil(2)].to_vec(),
                    v: yuv.v()[..su * h.div_ceil(2)].to_vec(),
                };
                if slot.put(picture) && proxy.send_event(UserEvent::Frame).is_err() {
                    return;
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("decode error ({e}), asking for a keyframe");
                let _ = outgoing.send(ClientMsg::RequestKeyframe);
            }
        }
    }
}
