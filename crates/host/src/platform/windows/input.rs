//! Client input as Windows mouse and keyboard events, placed on the shared monitor.

use fmp_proto::{InputEvent, MouseButton, TouchPhase, hid};
use windows::Win32::Foundation::RECT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::VirtualDisplay;

const WHEEL_DELTA: f32 = 120.0;
/// Wheel units per pixel of trackpad scrolling.
const WHEEL_PER_PIXEL: f32 = 3.0;

pub struct Injector {
    rect: RECT,
    touch_id: Option<u64>,
}

impl Injector {
    pub fn new(display: &VirtualDisplay) -> Self {
        Self { rect: display.rect, touch_id: None }
    }

    pub fn inject(&mut self, event: &InputEvent) {
        match *event {
            InputEvent::PointerMove { x, y } => self.move_to(x, y),
            InputEvent::PointerButton { button, pressed } => button_event(button, pressed),
            InputEvent::Scroll { dx, dy, precise } => {
                let per = if precise { WHEEL_PER_PIXEL } else { WHEEL_DELTA };
                // Positive dy scrolls up and positive dx scrolls left (macOS convention);
                // Windows' horizontal wheel is positive to the right.
                let (v, h) = ((dy * per).round() as i32, (-dx * per).round() as i32);
                if v != 0 {
                    mouse(0, 0, v, MOUSEEVENTF_WHEEL);
                }
                if h != 0 {
                    mouse(0, 0, h, MOUSEEVENTF_HWHEEL);
                }
            }
            InputEvent::Key { usage, pressed } => key(usage, pressed),
            // First finger acts as the left button, which covers taps and drags.
            InputEvent::Touch { id, phase, x, y } => {
                if self.touch_id.is_some_and(|t| t != id) {
                    return;
                }
                self.move_to(x, y);
                match phase {
                    TouchPhase::Started => {
                        self.touch_id = Some(id);
                        button_event(MouseButton::Left, true);
                    }
                    TouchPhase::Moved => {}
                    TouchPhase::Ended | TouchPhase::Cancelled => {
                        self.touch_id = None;
                        button_event(MouseButton::Left, false);
                    }
                }
            }
        }
    }

    /// `x`, `y` from 0 to 1 across the shared monitor.
    fn move_to(&self, x: f32, y: f32) {
        let r = self.rect;
        let px = r.left as f64 + x.clamp(0.0, 1.0) as f64 * (r.right - r.left - 1) as f64;
        let py = r.top as f64 + y.clamp(0.0, 1.0) as f64 * (r.bottom - r.top - 1) as f64;
        // SAFETY: plain system metric queries.
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let nx = ((px - vx as f64) * 65535.0 / (vw - 1).max(1) as f64).round() as i32;
        let ny = ((py - vy as f64) * 65535.0 / (vh - 1).max(1) as f64).round() as i32;
        mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
    }
}

fn button_event(button: MouseButton, pressed: bool) {
    let (flags, data) = match (button, pressed) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
        (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
        (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
        (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
    };
    mouse(0, 0, data, flags);
}

fn key(usage: u16, pressed: bool) {
    let Some((scan, extended)) = hid::set1_scancode(usage) else { return };
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !pressed {
        flags |= KEYEVENTF_KEYUP;
    }
    send(INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(0), wScan: scan, dwFlags: KEYBD_EVENT_FLAGS(flags.0), time: 0, dwExtraInfo: 0 },
        },
    });
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) {
    send(INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    });
}

fn send(input: INPUT) {
    // SAFETY: one fully initialised INPUT.
    unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
}
