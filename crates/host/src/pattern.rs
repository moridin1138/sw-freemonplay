//! A synthetic frame source for testing the pipeline without a virtual display or any OS
//! permissions: moving colour bars, plus a white square wherever the client's pointer is.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fmp_proto::InputEvent;

use crate::frame::{Frame, FrameSlot, I420Frame};

pub struct Pattern {
    stop: Arc<AtomicBool>,
    pointer: Arc<Mutex<(f32, f32)>>,
}

impl Pattern {
    pub fn start(width: u32, height: u32, fps: u32, slot: Arc<FrameSlot>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let pointer = Arc::new(Mutex::new((0.5, 0.5)));
        let (s, p) = (stop.clone(), pointer.clone());
        std::thread::spawn(move || {
            let (w, h) = (width as usize, height as usize);
            let interval = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
            let mut next = Instant::now();
            let mut t = 0usize;
            while !s.load(Ordering::Relaxed) {
                let (px, py) = *p.lock().unwrap();
                slot.push(Frame::I420(draw(w, h, t, px, py)));
                t += 1;
                next += interval;
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
            }
        });
        Self { stop, pointer }
    }

    pub fn inject(&self, event: &InputEvent) {
        // Never which keys: logs mustn't record what people type.
        if !matches!(event, InputEvent::Key { .. }) {
            tracing::debug!("input: {event:?}");
        }
        match *event {
            InputEvent::PointerMove { x, y } | InputEvent::Touch { x, y, .. } => *self.pointer.lock().unwrap() = (x, y),
            _ => {}
        }
    }
}

impl Drop for Pattern {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn draw(w: usize, h: usize, t: usize, px: f32, py: f32) -> I420Frame {
    // Eight vertical bars (BT.709 video range YUV) scrolling sideways.
    const BARS: [(u8, u8, u8); 8] = [
        (235, 128, 128), // white
        (219, 16, 138),  // yellow
        (188, 154, 16),  // cyan
        (173, 42, 26),   // green
        (78, 214, 230),  // magenta
        (63, 102, 240),  // red
        (32, 240, 118),  // blue
        (16, 128, 128),  // black
    ];
    let bar = |x: usize| BARS[((x + t * 4) * 8 / w) % 8];
    let (cw, ch) = (w / 2, h / 2);
    let mut y = vec![0; w * h];
    let mut u = vec![0; cw * ch];
    let mut v = vec![0; cw * ch];
    for row in 0..h {
        for col in 0..w {
            y[row * w + col] = bar(col).0;
        }
    }
    for row in 0..ch {
        for col in 0..cw {
            let (_, cb, cr) = bar(col * 2);
            u[row * cw + col] = cb;
            v[row * cw + col] = cr;
        }
    }
    // Pointer marker.
    let size = (w.min(h) / 20).max(8);
    let (cx, cy) = ((px * w as f32) as usize, (py * h as f32) as usize);
    for row in cy.saturating_sub(size / 2)..(cy + size / 2).min(h) {
        for col in cx.saturating_sub(size / 2)..(cx + size / 2).min(w) {
            y[row * w + col] = 235;
            u[(row / 2) * cw + col / 2] = 128;
            v[(row / 2) * cw + col / 2] = 128;
        }
    }
    I420Frame { width: w, height: h, y, u, v, captured_at: Instant::now() }
}
