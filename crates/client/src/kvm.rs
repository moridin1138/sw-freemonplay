//! Keyboard and mouse mode, client side: the host's keyboard and mouse control this
//! computer's own monitors (the ones set to "Keyboard and mouse").
//!
//! The host sends relative movement; we keep our own pointer position and place the cursor
//! absolutely, so this OS's pointer acceleration isn't applied on top of the host's. When
//! the pointer leaves our monitors toward the host (onto the monitor showing the host's
//! screen, or across the edge facing it), control goes back.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fmp_proto::{ClientMsg, Edge, HostMsg, KvmLayout, MouseButton, Rect, RemoteInput, ReturnAt, hid};

/// Whether this OS can be controlled yet.
pub fn supported() -> bool {
    imp::SUPPORTED
}

pub struct Remote {
    layout: KvmLayout,
    /// Pixels per host point, for each of `layout.monitors`.
    scales: Vec<f64>,
    /// Mac shortcuts: Command acts as Ctrl (and Ctrl as the Windows key).
    swap_command_control: bool,
    active: Option<Active>,
    held_keys: HashSet<u16>,
    held_buttons: HashSet<u8>,
    /// Last clipboard text synced either way.
    clipboard: Option<String>,
    injector: imp::Injector,
    /// Shared with the app: while set, input in our own windows (which the injected input
    /// may land in) mustn't be forwarded back to the host.
    in_control: Arc<AtomicBool>,
}

struct Active {
    x: f64,
    y: f64,
    back: Option<Edge>,
}

impl Remote {
    pub fn new(layout: KvmLayout, scales: Vec<f64>, swap_command_control: bool, in_control: Arc<AtomicBool>) -> Self {
        Self {
            layout,
            scales,
            swap_command_control,
            active: None,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            clipboard: None,
            injector: imp::Injector,
            in_control,
        }
    }

    /// Handles a message from the host; returns any to send back.
    pub fn handle(&mut self, msg: &HostMsg) -> Vec<ClientMsg> {
        match *msg {
            HostMsg::Clipboard(ref text) => {
                imp::set_clipboard(text);
                self.clipboard = Some(text.clone());
            }
            HostMsg::RemoteEnter { monitor, x, y, back } => {
                let Some(m) = self.layout.monitors.get(monitor as usize) else { return Vec::new() };
                let (x, y) = (m.x as f64 + x as f64 * m.width as f64, m.y as f64 + y as f64 * m.height as f64);
                tracing::info!("the host's keyboard and mouse are here");
                self.injector.move_to(x, y);
                self.active = Some(Active { x, y, back });
                self.in_control.store(true, Ordering::Relaxed);
            }
            HostMsg::RemoteLeave => {
                tracing::info!("the host took its keyboard and mouse back");
                self.deactivate();
            }
            HostMsg::Remote(input) if self.active.is_some() => return self.input(input),
            _ => {}
        }
        Vec::new()
    }

    fn input(&mut self, input: RemoteInput) -> Vec<ClientMsg> {
        match input {
            RemoteInput::Move { dx, dy } => return self.move_by(dx as f64, dy as f64),
            RemoteInput::Button { button, pressed } => {
                let id = button as u8;
                if pressed { self.held_buttons.insert(id) } else { self.held_buttons.remove(&id) };
                self.injector.button(button, pressed);
            }
            RemoteInput::Scroll { dx, dy, precise } => self.injector.scroll(dx, dy, precise),
            RemoteInput::Key { usage, pressed } => {
                let usage = if self.swap_command_control { swap_command_control(usage) } else { usage };
                if pressed { self.held_keys.insert(usage) } else { self.held_keys.remove(&usage) };
                self.injector.key(usage, pressed);
            }
        }
        Vec::new()
    }

    fn move_by(&mut self, dx: f64, dy: f64) -> Vec<ClientMsg> {
        let Some(active) = &self.active else { return Vec::new() };
        let (x, y, back) = (active.x, active.y, active.back);
        let scale = self.monitor_at(x, y).map_or(1.0, |i| self.scales.get(i).copied().unwrap_or(1.0));
        let (nx, ny) = (x + dx * scale, y + dy * scale);

        // Mixed layout: onto the monitor showing the host's screen.
        if let Some(screen) = self.layout.screen
            && screen.contains(nx, ny)
        {
            let at = ReturnAt::Screen {
                x: ((nx - screen.x as f64) / screen.width as f64) as f32,
                y: ((ny - screen.y as f64) / screen.height as f64) as f32,
            };
            return self.give_back(at);
        }
        if self.monitor_at(nx, ny).is_some() {
            return self.place(nx, ny);
        }
        // Off our monitors: across the edge facing the host goes back.
        if let Some(edge) = back
            && let Some(along) = crossed(&self.layout.monitors, edge, nx, ny)
        {
            return self.give_back(ReturnAt::Edge { along });
        }
        // Otherwise slide along the edge, as a local mouse would.
        if self.monitor_at(nx, y).is_some() {
            self.place(nx, y)
        } else if self.monitor_at(x, ny).is_some() {
            self.place(x, ny)
        } else {
            Vec::new()
        }
    }

    fn place(&mut self, x: f64, y: f64) -> Vec<ClientMsg> {
        if let Some(active) = &mut self.active {
            (active.x, active.y) = (x, y);
        }
        self.injector.move_to(x, y);
        Vec::new()
    }

    fn give_back(&mut self, at: ReturnAt) -> Vec<ClientMsg> {
        let mut out = Vec::new();
        if let Some(text) = imp::clipboard_text().filter(|t| self.clipboard.as_ref() != Some(t)) {
            self.clipboard = Some(text.clone());
            out.push(ClientMsg::Clipboard(text));
        }
        out.push(ClientMsg::ReturnControl(at));
        tracing::info!("keyboard and mouse back to the host");
        self.deactivate();
        out
    }

    /// Lets go of anything still held, so no key or button sticks down.
    fn deactivate(&mut self) {
        self.active = None;
        self.in_control.store(false, Ordering::Relaxed);
        for usage in std::mem::take(&mut self.held_keys) {
            self.injector.key(usage, false);
        }
        for id in std::mem::take(&mut self.held_buttons) {
            if let Some(button) = button_from_id(id) {
                self.injector.button(button, false);
            }
        }
    }

    fn monitor_at(&self, x: f64, y: f64) -> Option<usize> {
        self.layout.monitors.iter().position(|m| m.contains(x, y))
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.deactivate();
    }
}

/// Whether `(x, y)` lies beyond the monitors' outer `edge`, and how far along it (0 to 1).
fn crossed(monitors: &[Rect], edge: Edge, x: f64, y: f64) -> Option<f32> {
    let left = monitors.iter().map(|m| m.x).min()? as f64;
    let top = monitors.iter().map(|m| m.y).min()? as f64;
    let right = monitors.iter().map(Rect::right).max()? as f64;
    let bottom = monitors.iter().map(Rect::bottom).max()? as f64;
    let along_y = ((y - top) / (bottom - top)).clamp(0.0, 1.0) as f32;
    let along_x = ((x - left) / (right - left)).clamp(0.0, 1.0) as f32;
    match edge {
        Edge::Left if x < left => Some(along_y),
        Edge::Right if x >= right => Some(along_y),
        Edge::Top if y < top => Some(along_x),
        Edge::Bottom if y >= bottom => Some(along_x),
        _ => None,
    }
}

fn swap_command_control(usage: u16) -> u16 {
    match usage {
        hid::META_LEFT => hid::CONTROL_LEFT,
        hid::CONTROL_LEFT => hid::META_LEFT,
        hid::META_RIGHT => hid::CONTROL_RIGHT,
        hid::CONTROL_RIGHT => hid::META_RIGHT,
        other => other,
    }
}

fn button_from_id(id: u8) -> Option<MouseButton> {
    [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward]
        .into_iter()
        .find(|b| *b as u8 == id)
}

#[cfg(target_os = "windows")]
mod imp {
    use fmp_proto::{MouseButton, hid};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
        KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
        MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    };

    pub const SUPPORTED: bool = true;
    const WHEEL_DELTA: f32 = 120.0;
    /// Wheel units per host pixel of trackpad scrolling.
    const WHEEL_PER_PIXEL: f32 = 3.0;

    pub struct Injector;

    impl Injector {
        pub fn move_to(&self, x: f64, y: f64) {
            // SAFETY: plain system metric queries.
            let (vx, vy, vw, vh) = unsafe {
                (
                    GetSystemMetrics(SM_XVIRTUALSCREEN),
                    GetSystemMetrics(SM_YVIRTUALSCREEN),
                    GetSystemMetrics(SM_CXVIRTUALSCREEN),
                    GetSystemMetrics(SM_CYVIRTUALSCREEN),
                )
            };
            let nx = ((x - vx as f64) * 65535.0 / (vw - 1).max(1) as f64).round() as i32;
            let ny = ((y - vy as f64) * 65535.0 / (vh - 1).max(1) as f64).round() as i32;
            mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
        }

        pub fn button(&self, button: MouseButton, pressed: bool) {
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

        pub fn scroll(&self, dx: f32, dy: f32, precise: bool) {
            let per = if precise { WHEEL_PER_PIXEL } else { WHEEL_DELTA };
            // Host: positive dy scrolls up, positive dx scrolls left; Windows: positive
            // wheel scrolls up, positive horizontal wheel scrolls right.
            let (v, h) = ((dy * per).round() as i32, (-dx * per).round() as i32);
            if v != 0 {
                mouse(0, 0, v, MOUSEEVENTF_WHEEL);
            }
            if h != 0 {
                mouse(0, 0, h, MOUSEEVENTF_HWHEEL);
            }
        }

        pub fn key(&self, usage: u16, pressed: bool) {
            let Some((scan, extended)) = scancode(usage) else { return };
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

    /// USB HID usage to a set 1 scancode, and whether it's an extended (E0) key.
    fn scancode(usage: u16) -> Option<(u16, bool)> {
        // a..z in HID order
        const LETTERS: [u16; 26] = [
            0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
            0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
        ];
        // F1..F12, F13..F24
        const FKEYS: [u16; 12] = [0x3B, 0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x57, 0x58];
        const FKEYS_HIGH: [u16; 12] = [0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x76];
        // keypad 1..9, 0
        const NUMPAD: [u16; 10] = [0x4F, 0x50, 0x51, 0x4B, 0x4C, 0x4D, 0x47, 0x48, 0x49, 0x52];
        let plain = |s| Some((s, false));
        let ext = |s| Some((s, true));
        match usage {
            hid::A..=hid::Z => plain(LETTERS[(usage - hid::A) as usize]),
            // 1..9 are 0x02..0x0A, 0 is 0x0B
            hid::DIGIT1..=hid::DIGIT0 => plain(0x02 + usage - hid::DIGIT1),
            hid::F1..=hid::F12 => plain(FKEYS[(usage - hid::F1) as usize]),
            hid::F13..=hid::F24 => plain(FKEYS_HIGH[(usage - hid::F13) as usize]),
            hid::NUMPAD1..=hid::NUMPAD0 => plain(NUMPAD[(usage - hid::NUMPAD1) as usize]),
            hid::ENTER => plain(0x1C),
            hid::ESCAPE => plain(0x01),
            hid::BACKSPACE => plain(0x0E),
            hid::TAB => plain(0x0F),
            hid::SPACE => plain(0x39),
            hid::MINUS => plain(0x0C),
            hid::EQUAL => plain(0x0D),
            hid::BRACKET_LEFT => plain(0x1A),
            hid::BRACKET_RIGHT => plain(0x1B),
            hid::BACKSLASH => plain(0x2B),
            hid::SEMICOLON => plain(0x27),
            hid::QUOTE => plain(0x28),
            hid::BACKQUOTE => plain(0x29),
            hid::COMMA => plain(0x33),
            hid::PERIOD => plain(0x34),
            hid::SLASH => plain(0x35),
            hid::CAPS_LOCK => plain(0x3A),
            hid::PRINT_SCREEN => ext(0x37),
            hid::SCROLL_LOCK => plain(0x46),
            hid::INSERT => ext(0x52),
            hid::HOME => ext(0x47),
            hid::PAGE_UP => ext(0x49),
            hid::DELETE => ext(0x53),
            hid::END => ext(0x4F),
            hid::PAGE_DOWN => ext(0x51),
            hid::ARROW_RIGHT => ext(0x4D),
            hid::ARROW_LEFT => ext(0x4B),
            hid::ARROW_DOWN => ext(0x50),
            hid::ARROW_UP => ext(0x48),
            hid::NUM_LOCK => plain(0x45),
            hid::NUMPAD_DIVIDE => ext(0x35),
            hid::NUMPAD_MULTIPLY => plain(0x37),
            hid::NUMPAD_SUBTRACT => plain(0x4A),
            hid::NUMPAD_ADD => plain(0x4E),
            hid::NUMPAD_ENTER => ext(0x1C),
            hid::NUMPAD_DECIMAL => plain(0x53),
            hid::INTL_BACKSLASH => plain(0x56),
            hid::CONTEXT_MENU => ext(0x5D),
            hid::CONTROL_LEFT => plain(0x1D),
            hid::SHIFT_LEFT => plain(0x2A),
            hid::ALT_LEFT => plain(0x38),
            hid::META_LEFT => ext(0x5B),
            hid::CONTROL_RIGHT => ext(0x1D),
            hid::SHIFT_RIGHT => plain(0x36),
            hid::ALT_RIGHT => ext(0x38),
            hid::META_RIGHT => ext(0x5C),
            _ => None,
        }
    }

    pub fn clipboard_text() -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
    }

    pub fn set_clipboard(text: &str) {
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            tracing::warn!("couldn't set the clipboard: {e}");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn maps_keys_to_scancodes() {
            assert_eq!(scancode(hid::A), Some((0x1E, false)));
            assert_eq!(scancode(hid::DIGIT0), Some((0x0B, false)));
            assert_eq!(scancode(hid::ARROW_UP), Some((0x48, true)));
            assert_eq!(scancode(hid::F24), Some((0x76, false)));
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    //! Not yet: macOS (CGEvent) and Linux (libei or uinput) clients can't be controlled.
    use fmp_proto::MouseButton;

    pub const SUPPORTED: bool = false;

    pub struct Injector;

    impl Injector {
        pub fn move_to(&self, _x: f64, _y: f64) {}
        pub fn button(&self, _button: MouseButton, _pressed: bool) {}
        pub fn scroll(&self, _dx: f32, _dy: f32, _precise: bool) {}
        pub fn key(&self, _usage: u16, _pressed: bool) {}
    }

    pub fn clipboard_text() -> Option<String> {
        None
    }

    pub fn set_clipboard(_text: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect { x, y, width, height }
    }

    #[test]
    fn mixed_layout_returns_on_entering_the_screen() {
        // Keyboard and mouse monitor on the left of the streamed screen.
        let layout = KvmLayout { monitors: vec![rect(-1920, 0, 1920, 1080)], screen: Some(rect(0, 0, 2736, 1824)) };
        let mut remote = Remote::new(layout, vec![1.0], false, Arc::default());
        remote.handle(&HostMsg::RemoteEnter { monitor: 0, x: 0.99, y: 0.5, back: None });
        assert!(remote.move_by(-5.0, 0.0).is_empty());
        let out = remote.move_by(50.0, 0.0);
        assert!(matches!(out.last(), Some(ClientMsg::ReturnControl(ReturnAt::Screen { x, .. })) if *x < 0.02));
        assert!(remote.active.is_none());
    }

    #[test]
    fn beside_layout_returns_across_the_facing_edge_and_slides_elsewhere() {
        let layout = KvmLayout { monitors: vec![rect(0, 0, 1920, 1080)], screen: None };
        let mut remote = Remote::new(layout, vec![2.0], false, Arc::default());
        remote.handle(&HostMsg::RemoteEnter { monitor: 0, x: 0.0, y: 0.5, back: Some(Edge::Left) });
        // Up past the top: slides along it instead of leaving.
        assert!(remote.move_by(10.0, -1000.0).is_empty());
        let a = remote.active.as_ref().unwrap();
        assert_eq!((a.x, a.y), (20.0, 540.0));
        // Left past the edge facing the host: back.
        let out = remote.move_by(-20.0, 0.0);
        assert!(matches!(out.last(), Some(ClientMsg::ReturnControl(ReturnAt::Edge { along })) if (*along - 0.5).abs() < 0.01));
    }

    #[test]
    fn swaps_command_and_control() {
        assert_eq!(swap_command_control(hid::META_LEFT), hid::CONTROL_LEFT);
        assert_eq!(swap_command_control(hid::CONTROL_RIGHT), hid::META_RIGHT);
        assert_eq!(swap_command_control(hid::A), hid::A);
    }
}
