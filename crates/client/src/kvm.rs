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

use crate::clip::ClipSync;

/// Whether this OS can be controlled yet.
pub fn supported() -> bool {
    imp::SUPPORTED
}

/// Everything keyboard and mouse mode needs to know about this computer's monitors.
pub struct Setup {
    pub layout: KvmLayout,
    /// Pixels per host point, for each of `layout.monitors`.
    pub scales: Vec<f64>,
    /// The OS's id for each of `layout.monitors` (macOS display ids; unused elsewhere).
    pub displays: Vec<u32>,
    /// Mac shortcuts: Command acts as Ctrl (Windows and Linux only).
    pub swap_command_control: bool,
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
    /// Clipboard sharing, if on: the newest copied text goes back with control.
    clipboard: Option<Arc<ClipSync>>,
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
    pub fn new(setup: Setup, in_control: Arc<AtomicBool>, clipboard: Option<Arc<ClipSync>>) -> Self {
        let injector = imp::Injector::new(&setup.layout.monitors, &setup.displays);
        Self {
            layout: setup.layout,
            scales: setup.scales,
            // A Mac controlling a Mac needs no swapping.
            swap_command_control: setup.swap_command_control && !cfg!(target_os = "macos"),
            active: None,
            held_keys: HashSet::new(),
            held_buttons: HashSet::new(),
            clipboard,
            injector,
            in_control,
        }
    }

    /// Handles a message from the host; returns any to send back.
    pub fn handle(&mut self, msg: &HostMsg) -> Vec<ClientMsg> {
        match *msg {
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
        // So a paste on the host works right away, without waiting for the next poll.
        if let Some(item) = self.clipboard.as_ref().and_then(|c| c.copied()) {
            out.push(item.into_message());
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
        pub fn new(_monitors: &[fmp_proto::Rect], _displays: &[u32]) -> Self {
            Self
        }

        pub fn move_to(&mut self, x: f64, y: f64) {
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

        pub fn button(&mut self, button: MouseButton, pressed: bool) {
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

        pub fn scroll(&mut self, dx: f32, dy: f32, precise: bool) {
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

        pub fn key(&mut self, usage: u16, pressed: bool) {
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


}

#[cfg(target_os = "macos")]
mod imp {
    //! CoreGraphics events. Needs the Accessibility permission for this program (or the
    //! terminal running it).

    use std::ffi::c_void;
    use std::ptr::null;
    use std::time::{Duration, Instant};

    use fmp_proto::{MouseButton, Rect, hid};

    pub const SUPPORTED: bool = true;
    const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(500);

    #[repr(C)]
    #[derive(Clone, Copy, Default, PartialEq)]
    struct CGPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    struct CGRect {
        origin: CGPoint,
        size: CGPoint,
    }
    type CGEventRef = *mut c_void;

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGDisplayBounds(display: u32) -> CGRect;
        fn CGEventCreateMouseEvent(source: *const c_void, kind: u32, pos: CGPoint, button: u32) -> CGEventRef;
        fn CGEventCreateKeyboardEvent(source: *const c_void, keycode: u16, down: bool) -> CGEventRef;
        fn CGEventCreateScrollWheelEvent2(source: *const c_void, units: u32, count: u32, w1: i32, w2: i32, w3: i32) -> CGEventRef;
        fn CGEventSetFlags(event: CGEventRef, flags: u64);
        fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
        fn CGEventPost(tap: u32, event: CGEventRef);
        fn CGPreflightPostEventAccess() -> bool;
        fn CGRequestPostEventAccess() -> bool;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(cf: *const c_void);
    }

    pub struct Injector {
        /// Our monitors (desktop pixels) and their display ids.
        monitors: Vec<(Rect, u32)>,
        pos: CGPoint,
        held: Option<MouseButton>,
        modifiers: u64,
        last_click: Option<(MouseButton, Instant, CGPoint)>,
        click_count: i64,
    }

    impl Injector {
        pub fn new(monitors: &[Rect], displays: &[u32]) -> Self {
            // SAFETY: permission queries; the request shows the system prompt once.
            if !unsafe { CGPreflightPostEventAccess() } {
                unsafe { CGRequestPostEventAccess() };
                tracing::warn!(
                    "the host's keyboard and mouse need the Accessibility permission: allow freemonplay-screen \
                     (or the terminal running it) in System Settings > Privacy & Security > Accessibility"
                );
            }
            Self {
                monitors: monitors.iter().copied().zip(displays.iter().copied()).collect(),
                pos: CGPoint::default(),
                held: None,
                modifiers: 0,
                last_click: None,
                click_count: 1,
            }
        }

        /// Desktop pixels to global points, through the display the point is on.
        fn to_global(&self, x: f64, y: f64) -> Option<CGPoint> {
            let &(m, id) = self.monitors.iter().find(|(m, _)| m.contains(x, y))?;
            // SAFETY: plain query.
            let b = unsafe { CGDisplayBounds(id) };
            Some(CGPoint {
                x: b.origin.x + (x - m.x as f64) / m.width as f64 * b.size.x,
                y: b.origin.y + (y - m.y as f64) / m.height as f64 * b.size.y,
            })
        }

        pub fn move_to(&mut self, x: f64, y: f64) {
            let Some(pos) = self.to_global(x, y) else { return };
            self.pos = pos;
            let (kind, button) = match self.held {
                Some(MouseButton::Left) => (6, 0),
                Some(MouseButton::Right) => (7, 1),
                Some(b) => (27, cg_button(b)),
                None => (5, 0),
            };
            // SAFETY: creating, posting and releasing our own event.
            unsafe { self.post(CGEventCreateMouseEvent(null(), kind, self.pos, button)) }
        }

        pub fn button(&mut self, button: MouseButton, pressed: bool) {
            let kind = match (button, pressed) {
                (MouseButton::Left, true) => 1,
                (MouseButton::Left, false) => 2,
                (MouseButton::Right, true) => 3,
                (MouseButton::Right, false) => 4,
                (_, true) => 25,
                (_, false) => 26,
            };
            if pressed {
                // macOS needs an explicit click count to see double and triple clicks.
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
                let e = CGEventCreateMouseEvent(null(), kind, self.pos, cg_button(button));
                if !e.is_null() {
                    CGEventSetIntegerValueField(e, 1, self.click_count);
                }
                self.post(e);
            }
        }

        pub fn scroll(&mut self, dx: f32, dy: f32, precise: bool) {
            let units = if precise { 0 } else { 1 };
            // SAFETY: as above.
            unsafe { self.post(CGEventCreateScrollWheelEvent2(null(), units, 2, dy.round() as i32, dx.round() as i32, 0)) }
        }

        pub fn key(&mut self, usage: u16, pressed: bool) {
            let Some(code) = hid::mac_keycode(usage) else { return };
            let flag = match usage {
                hid::SHIFT_LEFT | hid::SHIFT_RIGHT => 0x0002_0000,
                hid::CONTROL_LEFT | hid::CONTROL_RIGHT => 0x0004_0000,
                hid::ALT_LEFT | hid::ALT_RIGHT => 0x0008_0000,
                hid::META_LEFT | hid::META_RIGHT => 0x0010_0000,
                _ => 0,
            };
            if pressed { self.modifiers |= flag } else { self.modifiers &= !flag }
            // SAFETY: as above.
            unsafe { self.post(CGEventCreateKeyboardEvent(null(), code, pressed)) }
        }

        /// Applies the held modifiers, posts and releases. Tolerates null.
        unsafe fn post(&self, event: CGEventRef) {
            if event.is_null() {
                return;
            }
            // SAFETY: a live event we own one reference to.
            unsafe {
                CGEventSetFlags(event, self.modifiers);
                CGEventPost(0, event);
                CFRelease(event);
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
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    //! Not yet: Linux clients (libei, or uinput) can't be controlled.
    use fmp_proto::{MouseButton, Rect};

    pub const SUPPORTED: bool = false;

    pub struct Injector;

    impl Injector {
        pub fn new(_monitors: &[Rect], _displays: &[u32]) -> Self {
            Self
        }
        pub fn move_to(&mut self, _x: f64, _y: f64) {}
        pub fn button(&mut self, _button: MouseButton, _pressed: bool) {}
        pub fn scroll(&mut self, _dx: f32, _dy: f32, _precise: bool) {}
        pub fn key(&mut self, _usage: u16, _pressed: bool) {}
    }
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
        let setup = Setup { layout, scales: vec![1.0], displays: vec![0], swap_command_control: false };
        let mut remote = Remote::new(setup, Arc::default(), None);
        remote.handle(&HostMsg::RemoteEnter { monitor: 0, x: 0.99, y: 0.5, back: None });
        assert!(remote.move_by(-5.0, 0.0).is_empty());
        let out = remote.move_by(50.0, 0.0);
        assert!(matches!(out.last(), Some(ClientMsg::ReturnControl(ReturnAt::Screen { x, .. })) if *x < 0.02));
        assert!(remote.active.is_none());
    }

    #[test]
    fn beside_layout_returns_across_the_facing_edge_and_slides_elsewhere() {
        let layout = KvmLayout { monitors: vec![rect(0, 0, 1920, 1080)], screen: None };
        let setup = Setup { layout, scales: vec![2.0], displays: vec![0], swap_command_control: false };
        let mut remote = Remote::new(setup, Arc::default(), None);
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
