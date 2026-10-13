//! Windows side of keyboard and mouse mode: low-level mouse and keyboard hooks that watch
//! the pointer and, while a screen has control, swallow local input and forward it (see
//! `crate::kvm`). No special permission is needed.
//!
//! While a screen has control the cursor stays parked just inside the edge it left by, and
//! each mouse movement is measured from there (so it keeps Windows' pointer acceleration).
//! It's hidden by swapping the system cursors for a blank one, restored from the user's
//! cursor scheme afterwards.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, bail};
use fmp_proto::{MouseButton, RemoteInput, hid};
use windows::Win32::Foundation::{HMODULE, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateCursor, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED,
    LLKHF_UP, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SPI_SETCURSORS, SYSTEM_CURSOR_ID, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SetCursorPos, SetSystemCursor, SetWindowsHookExW, SystemParametersInfoW, TranslateMessage, WH_KEYBOARD_LL,
    WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use crate::kvm::{Bounds, Kvm, Pointer};

static KVM: OnceLock<Arc<Kvm>> = OnceLock::new();
/// Where the cursor is parked while a screen has control.
static PARK: Mutex<(i32, i32)> = Mutex::new((0, 0));
/// The last pointer position the mouse hook saw (to tell which way it's moving).
static LAST: Mutex<Option<(i32, i32)>> = Mutex::new(None);
/// Keys pressed while a screen has control (forwarded, not seen here).
static HELD: Mutex<Option<HashSet<u16>>> = Mutex::new(None);
static HIDDEN: AtomicBool = AtomicBool::new(false);

/// The system cursors swapped for a blank one while a screen has the mouse (OCR_*).
const CURSORS: [u32; 14] = [32512, 32513, 32514, 32515, 32516, 32642, 32643, 32644, 32645, 32646, 32648, 32649, 32650, 32651];
const WHEEL_DELTA: f32 = 120.0;
const XBUTTON1: u32 = 1;
/// K: Ctrl+Alt+Shift+K takes the keyboard and mouse back.
const ESCAPE_KEY: u16 = hid::A + (b'k' - b'a') as u16;

/// Starts watching the keyboard and mouse (once; later calls do nothing).
pub fn start(kvm: Arc<Kvm>) -> Result<()> {
    if KVM.set(kvm).is_err() {
        return Ok(());
    }
    // A blank cursor left over from a crash while a screen had the mouse.
    restore_cursors();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("kvm-hooks".into()).spawn(move || {
        // SAFETY: global low-level hooks owned by this thread, whose message loop (which
        // runs them) lasts for the life of the process.
        unsafe {
            let module = GetModuleHandleW(None).unwrap_or(HMODULE::default());
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(module.into()), 0);
            let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), Some(module.into()), 0);
            let _ = tx.send(mouse.is_ok() && keyboard.is_ok());
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    })?;
    match rx.recv() {
        Ok(true) => {
            tracing::info!("watching the pointer for keyboard and mouse mode");
            Ok(())
        }
        _ => bail!("couldn't watch the keyboard and mouse"),
    }
}

/// Pass the event on to the rest of the system.
fn pass(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: handing the event to the next hook unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Keep the event from the rest of the system (a screen has it).
const SWALLOW: LRESULT = LRESULT(1);

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(kvm) = KVM.get() else { return pass(code, wparam, lparam) };
    if code < 0 {
        return pass(code, wparam, lparam);
    }
    // SAFETY: for low-level mouse hooks, lparam points to an MSLLHOOKSTRUCT.
    let m = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    // Our own injected input (screens controlling this PC) isn't the local mouse.
    if m.flags & LLMHF_INJECTED != 0 {
        return pass(code, wparam, lparam);
    }
    let message = wparam.0 as u32;
    let (x, y) = (m.pt.x, m.pt.y);

    if !kvm.is_active() {
        let last = LAST.lock().unwrap().replace((x, y));
        if message == WM_MOUSEMOVE
            && let Some(pointer) = pointer_at(x, y, last)
            && kvm.check_entry(&pointer)
        {
            return SWALLOW;
        }
        return pass(code, wparam, lparam);
    }

    // A screen has the mouse: forward, and keep the cursor parked here.
    let wheel = (m.mouseData >> 16) as u16 as i16 as f32 / WHEEL_DELTA;
    let input = match message {
        WM_MOUSEMOVE => {
            let park = *PARK.lock().unwrap();
            let (dx, dy) = (x - park.0, y - park.1);
            (dx != 0 || dy != 0).then_some(RemoteInput::Move { dx: dx as f32, dy: dy as f32 })
        }
        WM_LBUTTONDOWN | WM_LBUTTONUP => Some(RemoteInput::Button { button: MouseButton::Left, pressed: message == WM_LBUTTONDOWN }),
        WM_RBUTTONDOWN | WM_RBUTTONUP => Some(RemoteInput::Button { button: MouseButton::Right, pressed: message == WM_RBUTTONDOWN }),
        WM_MBUTTONDOWN | WM_MBUTTONUP => Some(RemoteInput::Button { button: MouseButton::Middle, pressed: message == WM_MBUTTONDOWN }),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let button = if m.mouseData >> 16 == XBUTTON1 { MouseButton::Back } else { MouseButton::Forward };
            Some(RemoteInput::Button { button, pressed: message == WM_XBUTTONDOWN })
        }
        // Positive is up; screens take macOS's convention (positive dx scrolls left).
        WM_MOUSEWHEEL => Some(RemoteInput::Scroll { dx: 0.0, dy: wheel, precise: false }),
        WM_MOUSEHWHEEL => Some(RemoteInput::Scroll { dx: -wheel, dy: 0.0, precise: false }),
        _ => None,
    };
    if let Some(input) = input {
        kvm.forward(input);
    }
    SWALLOW
}

/// The pointer for `Kvm::check_entry`: its monitor, and which way it's pushing. When it's
/// held at an edge Windows reports the same position again, so no movement at an edge
/// counts as pushing against it.
fn pointer_at(x: i32, y: i32, last: Option<(i32, i32)>) -> Option<Pointer> {
    // SAFETY: a plain query.
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    let bounds = monitor_bounds(monitor)?;
    let (mut dx, mut dy) = last.map_or((0, 0), |(lx, ly)| (x - lx, y - ly));
    if dx == 0 && dy == 0 {
        let (right, bottom) = ((bounds.x + bounds.width) as i32 - 1, (bounds.y + bounds.height) as i32 - 1);
        dx = if x >= right { 1 } else if x <= bounds.x as i32 { -1 } else { 0 };
        dy = if y >= bottom { 1 } else if y <= bounds.y as i32 { -1 } else { 0 };
    }
    Some(Pointer { display: monitor.0 as usize as u32, bounds, x: x as f64, y: y as f64, dx: dx as f64, dy: dy as f64 })
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(kvm) = KVM.get() else { return pass(code, wparam, lparam) };
    if code < 0 || !kvm.is_active() {
        return pass(code, wparam, lparam);
    }
    // SAFETY: for low-level keyboard hooks, lparam points to a KBDLLHOOKSTRUCT.
    let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if k.flags.0 & LLKHF_INJECTED.0 != 0 {
        return pass(code, wparam, lparam);
    }
    let Some(usage) = hid_usage(k.scanCode as u16, k.flags.0 & LLKHF_EXTENDED.0 != 0) else { return SWALLOW };
    let pressed = k.flags.0 & LLKHF_UP.0 == 0;
    let mut held = HELD.lock().unwrap();
    let held = held.get_or_insert_with(HashSet::new);
    if pressed {
        if usage == ESCAPE_KEY && escape_held(held) {
            held.clear();
            kvm.release("Ctrl+Alt+Shift+K");
            return SWALLOW;
        }
        // Held keys repeat; the screen's OS repeats them itself.
        if held.insert(usage) {
            kvm.forward(RemoteInput::Key { usage, pressed: true });
        }
        SWALLOW
    } else if held.remove(&usage) {
        kvm.forward(RemoteInput::Key { usage, pressed: false });
        SWALLOW
    } else {
        // Pressed before the screen took over: let this PC see it let go.
        pass(code, wparam, lparam)
    }
}

fn escape_held(held: &HashSet<u16>) -> bool {
    let any = |a, b| held.contains(&a) || held.contains(&b);
    any(hid::CONTROL_LEFT, hid::CONTROL_RIGHT) && any(hid::ALT_LEFT, hid::ALT_RIGHT) && any(hid::SHIFT_LEFT, hid::SHIFT_RIGHT)
}

/// Set 1 scancode (and whether it's extended) to USB HID usage, the inverse of
/// `hid::set1_scancode`.
fn hid_usage(scancode: u16, extended: bool) -> Option<u16> {
    static TABLE: OnceLock<HashMap<(u16, bool), u16>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = HashMap::new();
        for usage in 0..=0xE7u16 {
            if let Some(code) = hid::set1_scancode(usage) {
                table.entry(code).or_insert(usage);
            }
        }
        table
    });
    table.get(&(scancode, extended)).copied()
}

fn monitor_bounds(monitor: HMONITOR) -> Option<Bounds> {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: a sized MONITORINFO.
    unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool().then(|| {
        let r = info.rcMonitor;
        Bounds { x: r.left as f64, y: r.top as f64, width: (r.right - r.left) as f64, height: (r.bottom - r.top) as f64 }
    })
}

/// A monitor's desktop area, in physical pixels (the id is its HMONITOR, as
/// `VirtualDisplay::id` gives it).
pub fn display_bounds(display: u32) -> Option<Bounds> {
    monitor_bounds(HMONITOR(display as usize as *mut _))
}

pub fn main_display() -> u32 {
    // SAFETY: a plain query.
    unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }.0 as usize as u32
}

/// Parks the cursor at `park` and hides it while a screen has the mouse.
pub fn capture_cursor(park: (f64, f64)) {
    let park = (park.0.round() as i32, park.1.round() as i32);
    *PARK.lock().unwrap() = park;
    *HELD.lock().unwrap() = Some(HashSet::new());
    // SAFETY: plain cursor calls.
    unsafe {
        let _ = SetCursorPos(park.0, park.1);
    }
    if !HIDDEN.swap(true, Ordering::SeqCst) {
        hide_cursors();
    }
}

/// Shows the cursor again and puts it at `at`.
pub fn release_cursor(at: (f64, f64)) {
    *HELD.lock().unwrap() = None;
    *LAST.lock().unwrap() = None;
    // SAFETY: as above.
    unsafe {
        let _ = SetCursorPos(at.0.round() as i32, at.1.round() as i32);
    }
    if HIDDEN.swap(false, Ordering::SeqCst) {
        restore_cursors();
    }
}

/// Swaps every system cursor for a blank one (a background program can't otherwise hide
/// the cursor over other programs' windows).
fn hide_cursors() {
    // 32x32 cursor: all-ones AND mask and all-zeros XOR mask leave the screen unchanged.
    let and = [0xFFu8; 128];
    let xor = [0u8; 128];
    for id in CURSORS {
        // SAFETY: SetSystemCursor takes ownership of (and later destroys) each new cursor.
        unsafe {
            if let Ok(blank) = CreateCursor(None, 0, 0, 32, 32, and.as_ptr().cast(), xor.as_ptr().cast()) {
                let _ = SetSystemCursor(blank, SYSTEM_CURSOR_ID(id));
            }
        }
    }
}

/// Reloads the user's cursor scheme.
fn restore_cursors() {
    // SAFETY: a system setting reload with no data.
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes_map_back_to_hid() {
        assert_eq!(hid_usage(0x1E, false), Some(hid::A));
        assert_eq!(hid_usage(0x1C, false), Some(hid::ENTER));
        assert_eq!(hid_usage(0x1D, false), Some(hid::CONTROL_LEFT));
        assert_eq!(hid_usage(0x1D, true), Some(hid::CONTROL_RIGHT));
        assert_eq!(hid_usage(0x48, true), Some(hid::ARROW_UP));
    }

    #[test]
    fn escape_needs_control_alt_and_shift() {
        let held: HashSet<u16> = [hid::CONTROL_LEFT, hid::ALT_RIGHT, hid::SHIFT_LEFT].into();
        assert!(escape_held(&held));
        let held: HashSet<u16> = [hid::CONTROL_LEFT, hid::SHIFT_LEFT].into();
        assert!(!escape_held(&held));
    }
}
