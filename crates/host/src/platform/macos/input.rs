//! Injects client input as CoreGraphics events positioned on the virtual display.

use std::ptr::null;
use std::time::{Duration, Instant};

use fmp_proto::{InputEvent, MouseButton, TouchPhase, hid};

use super::VirtualDisplay;
use super::cg::{self, CGPoint};

const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(500);

pub struct Injector {
    display_id: u32,
    pos: CGPoint,
    held: Option<MouseButton>,
    modifiers: u64,
    touch_id: Option<u64>,
    last_click: Option<(MouseButton, Instant, CGPoint)>,
    click_count: i64,
}

impl Injector {
    pub fn new(display: &VirtualDisplay) -> Self {
        Self {
            display_id: display.id(),
            pos: CGPoint::default(),
            held: None,
            modifiers: 0,
            touch_id: None,
            last_click: None,
            click_count: 1,
        }
    }

    pub fn inject(&mut self, event: &InputEvent) {
        match *event {
            InputEvent::PointerMove { x, y } => {
                self.pos = self.to_global(x, y);
                self.post_move();
            }
            InputEvent::PointerButton { button, pressed } => self.post_button(button, pressed),
            InputEvent::Scroll { dx, dy, precise } => {
                let units = if precise { cg::kCGScrollEventUnitPixel } else { cg::kCGScrollEventUnitLine };
                // SAFETY: creating and posting an event we own, then releasing it.
                unsafe {
                    let e = cg::CGEventCreateScrollWheelEvent2(null(), units, 2, dy.round() as i32, dx.round() as i32, 0);
                    self.post(e);
                }
            }
            InputEvent::Key { usage, pressed } => {
                let Some(code) = hid::mac_keycode(usage) else { return };
                if let Some(flag) = modifier_flag(usage) {
                    if pressed { self.modifiers |= flag } else { self.modifiers &= !flag }
                }
                // SAFETY: as above.
                unsafe { self.post(cg::CGEventCreateKeyboardEvent(null(), code, pressed)) }
            }
            // First finger acts as a left mouse button, which covers taps and drags.
            InputEvent::Touch { id, phase, x, y } => {
                if self.touch_id.is_some_and(|t| t != id) {
                    return;
                }
                self.pos = self.to_global(x, y);
                match phase {
                    TouchPhase::Started => {
                        self.touch_id = Some(id);
                        self.post_move();
                        self.post_button(MouseButton::Left, true);
                    }
                    TouchPhase::Moved => self.post_move(),
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        self.touch_id = None;
                        self.post_button(MouseButton::Left, false);
                    }
                }
            }
        }
    }

    fn to_global(&self, x: f32, y: f32) -> CGPoint {
        // Re-read every time: the user can rearrange displays while we're connected.
        // SAFETY: plain query.
        let b = unsafe { cg::CGDisplayBounds(self.display_id) };
        let (x, y) = (x.clamp(0.0, 1.0) as f64, y.clamp(0.0, 1.0) as f64);
        CGPoint {
            x: b.origin.x + x * (b.size.width - 1.0).max(0.0),
            y: b.origin.y + y * (b.size.height - 1.0).max(0.0),
        }
    }

    fn post_move(&mut self) {
        let (kind, button) = match self.held {
            Some(MouseButton::Left) => (cg::kCGEventLeftMouseDragged, 0),
            Some(MouseButton::Right) => (cg::kCGEventRightMouseDragged, 1),
            Some(b) => (cg::kCGEventOtherMouseDragged, cg_button(b)),
            None => (cg::kCGEventMouseMoved, 0),
        };
        // SAFETY: as above.
        unsafe { self.post(cg::CGEventCreateMouseEvent(null(), kind, self.pos, button)) }
    }

    fn post_button(&mut self, button: MouseButton, pressed: bool) {
        let kind = match (button, pressed) {
            (MouseButton::Left, true) => cg::kCGEventLeftMouseDown,
            (MouseButton::Left, false) => cg::kCGEventLeftMouseUp,
            (MouseButton::Right, true) => cg::kCGEventRightMouseDown,
            (MouseButton::Right, false) => cg::kCGEventRightMouseUp,
            (_, true) => cg::kCGEventOtherMouseDown,
            (_, false) => cg::kCGEventOtherMouseUp,
        };
        if pressed {
            // macOS needs an explicit click count to recognise double and triple clicks.
            let now = Instant::now();
            let repeat = self.last_click.is_some_and(|(b, t, p)| {
                b == button && now - t < DOUBLE_CLICK_TIME && (p.x - self.pos.x).abs() < 4.0 && (p.y - self.pos.y).abs() < 4.0
            });
            self.click_count = if repeat { self.click_count + 1 } else { 1 };
            self.last_click = Some((button, now, self.pos));
            self.held = Some(button);
        } else if self.held == Some(button) {
            self.held = None;
        }
        // SAFETY: as above.
        unsafe {
            let e = cg::CGEventCreateMouseEvent(null(), kind, self.pos, cg_button(button));
            if !e.is_null() {
                cg::CGEventSetIntegerValueField(e, cg::kCGMouseEventClickState, self.click_count);
            }
            self.post(e);
        }
    }

    /// Applies our modifier state, posts and releases. Tolerates null (creation failed).
    unsafe fn post(&self, event: cg::CGEventRef) {
        if event.is_null() {
            return;
        }
        // SAFETY: `event` is a live CGEvent we created and own one reference to.
        unsafe {
            cg::CGEventSetFlags(event, self.modifiers);
            cg::CGEventPost(cg::kCGHIDEventTap, event);
            cg::CFRelease(event);
        }
    }
}

fn cg_button(b: MouseButton) -> u32 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::Back => 3,
        MouseButton::Forward => 4,
    }
}

fn modifier_flag(usage: u16) -> Option<u64> {
    Some(match usage {
        hid::SHIFT_LEFT | hid::SHIFT_RIGHT => cg::kCGEventFlagMaskShift,
        hid::CONTROL_LEFT | hid::CONTROL_RIGHT => cg::kCGEventFlagMaskControl,
        hid::ALT_LEFT | hid::ALT_RIGHT => cg::kCGEventFlagMaskAlternate,
        hid::META_LEFT | hid::META_RIGHT => cg::kCGEventFlagMaskCommand,
        // Caps Lock toggles rather than being held, so the OS tracks it, not us.
        _ => return None,
    })
}

