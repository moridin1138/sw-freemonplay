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
                let Some(code) = mac_keycode(usage) else { return };
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

/// HID usage to macOS virtual key code (kVK_* from HIToolbox/Events.h).
pub(super) fn mac_keycode(usage: u16) -> Option<u16> {
    // a..z in HID order
    const LETTERS: [u16; 26] = [
        0x00, 0x0B, 0x08, 0x02, 0x0E, 0x03, 0x05, 0x04, 0x22, 0x26, 0x28, 0x25, 0x2E, 0x2D, 0x1F, 0x23, 0x0C, 0x0F,
        0x01, 0x11, 0x20, 0x09, 0x0D, 0x07, 0x10, 0x06,
    ];
    // 1..9, 0
    const DIGITS: [u16; 10] = [0x12, 0x13, 0x14, 0x15, 0x17, 0x16, 0x1A, 0x1C, 0x19, 0x1D];
    // F1..F12
    const FKEYS: [u16; 12] = [0x7A, 0x78, 0x63, 0x76, 0x60, 0x61, 0x62, 0x64, 0x65, 0x6D, 0x67, 0x6F];
    // F13..F20 (macOS has no F21..F24)
    const FKEYS_HIGH: [u16; 8] = [0x69, 0x6B, 0x71, 0x6A, 0x40, 0x4F, 0x50, 0x5A];
    // keypad 1..9, 0
    const NUMPAD: [u16; 10] = [0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5B, 0x5C, 0x52];

    Some(match usage {
        hid::A..=hid::Z => LETTERS[(usage - hid::A) as usize],
        hid::DIGIT1..=hid::DIGIT0 => DIGITS[(usage - hid::DIGIT1) as usize],
        hid::F1..=hid::F12 => FKEYS[(usage - hid::F1) as usize],
        hid::F13..=hid::F24 => *FKEYS_HIGH.get((usage - hid::F13) as usize)?,
        hid::NUMPAD1..=hid::NUMPAD0 => NUMPAD[(usage - hid::NUMPAD1) as usize],
        hid::ENTER => 0x24,
        hid::ESCAPE => 0x35,
        hid::BACKSPACE => 0x33,
        hid::TAB => 0x30,
        hid::SPACE => 0x31,
        hid::MINUS => 0x1B,
        hid::EQUAL => 0x18,
        hid::BRACKET_LEFT => 0x21,
        hid::BRACKET_RIGHT => 0x1E,
        hid::BACKSLASH => 0x2A,
        hid::SEMICOLON => 0x29,
        hid::QUOTE => 0x27,
        hid::BACKQUOTE => 0x32,
        hid::COMMA => 0x2B,
        hid::PERIOD => 0x2F,
        hid::SLASH => 0x2C,
        hid::CAPS_LOCK => 0x39,
        hid::INSERT => 0x72, // Help
        hid::HOME => 0x73,
        hid::PAGE_UP => 0x74,
        hid::DELETE => 0x75,
        hid::END => 0x77,
        hid::PAGE_DOWN => 0x79,
        hid::ARROW_RIGHT => 0x7C,
        hid::ARROW_LEFT => 0x7B,
        hid::ARROW_DOWN => 0x7D,
        hid::ARROW_UP => 0x7E,
        hid::NUM_LOCK => 0x47, // Clear
        hid::NUMPAD_DIVIDE => 0x4B,
        hid::NUMPAD_MULTIPLY => 0x43,
        hid::NUMPAD_SUBTRACT => 0x4E,
        hid::NUMPAD_ADD => 0x45,
        hid::NUMPAD_ENTER => 0x4C,
        hid::NUMPAD_DECIMAL => 0x41,
        hid::NUMPAD_EQUAL => 0x51,
        hid::INTL_BACKSLASH => 0x0A,
        hid::CONTEXT_MENU => 0x6E,
        hid::CONTROL_LEFT => 0x3B,
        hid::SHIFT_LEFT => 0x38,
        hid::ALT_LEFT => 0x3A,
        hid::META_LEFT => 0x37,
        hid::CONTROL_RIGHT => 0x3E,
        hid::SHIFT_RIGHT => 0x3C,
        hid::ALT_RIGHT => 0x3D,
        hid::META_RIGHT => 0x36,
        _ => return None,
    })
}
