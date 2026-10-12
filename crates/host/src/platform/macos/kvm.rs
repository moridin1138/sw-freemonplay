//! macOS side of keyboard and mouse mode: an event tap that watches the pointer and, while
//! a client has control, swallows local input and forwards it (see `crate::kvm`).
//!
//! Needs the Accessibility permission (the same one client input already needs).

use std::ffi::c_void;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Result, bail};
use fmp_proto::{MouseButton, RemoteInput};

use super::cg::{self, CGPoint};
use crate::kvm::{Bounds, Kvm, Pointer};

static KVM: OnceLock<Arc<Kvm>> = OnceLock::new();
static TAP: Mutex<usize> = Mutex::new(0);
static HIDDEN: AtomicBool = AtomicBool::new(false);
/// Modifier keys held (device-dependent flag bits), to turn flags-changed events into
/// presses and releases.
static MODIFIERS: AtomicU64 = AtomicU64::new(0);

// Event types and fields (CGEventTypes.h).
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const SCROLL_WHEEL: u32 = 22;
const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
const FIELD_BUTTON_NUMBER: u32 = 3;
const FIELD_DELTA_X: u32 = 4;
const FIELD_DELTA_Y: u32 = 5;
const FIELD_AUTOREPEAT: u32 = 8;
const FIELD_KEYCODE: u32 = 9;
const FIELD_SCROLL_LINES_Y: u32 = 11;
const FIELD_SCROLL_LINES_X: u32 = 12;
const FIELD_SOURCE_PID: u32 = 41;
const FIELD_SCROLL_CONTINUOUS: u32 = 88;
const FIELD_SCROLL_POINTS_Y: u32 = 96;
const FIELD_SCROLL_POINTS_X: u32 = 97;
const SESSION_EVENT_TAP: u32 = 1;
const HEAD_INSERT: u32 = 0;
const TAP_OPTION_DEFAULT: u32 = 0;

/// Escape shortcut: Ctrl+Alt+Shift+K takes the keyboard and mouse back.
const ESCAPE_KEYCODE: i64 = 0x28;

pub type CFMachPortRef = *mut c_void;
type TapCallback = extern "C" fn(*mut c_void, u32, cg::CGEventRef, *mut c_void) -> cg::CGEventRef;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, mask: u64, callback: TapCallback, info: *mut c_void) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetLocation(event: cg::CGEventRef) -> CGPoint;
    fn CGEventGetFlags(event: cg::CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: cg::CGEventRef, field: u32) -> i64;
    fn CGEventGetDoubleValueField(event: cg::CGEventRef, field: u32) -> f64;
    fn CGGetDisplaysWithPoint(point: CGPoint, max: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
    fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    fn CGDisplayHideCursor(display: u32) -> i32;
    fn CGDisplayShowCursor(display: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopCommonModes: *const c_void;
    fn CFMachPortCreateRunLoopSource(allocator: *const c_void, port: CFMachPortRef, order: isize) -> *mut c_void;
    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopAddSource(rl: *mut c_void, source: *mut c_void, mode: *const c_void);
    fn CFRunLoopRun();
}

/// Starts watching the pointer (once; later calls do nothing).
pub fn start(kvm: Arc<Kvm>) -> Result<()> {
    if KVM.set(kvm).is_err() {
        return Ok(());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new().name("kvm-tap".into()).spawn(move || {
        let mask = [1u32, 2, 3, 4, 5, 6, 7, KEY_DOWN, KEY_UP, FLAGS_CHANGED, SCROLL_WHEEL, 25, 26, 27]
            .iter()
            .fold(0u64, |m, t| m | 1 << t);
        // SAFETY: standard event tap setup on this thread's run loop, which then runs for
        // the life of the process.
        unsafe {
            let tap = CGEventTapCreate(SESSION_EVENT_TAP, HEAD_INSERT, TAP_OPTION_DEFAULT, mask, on_event, null_mut());
            if tap.is_null() {
                let _ = tx.send(false);
                return;
            }
            *TAP.lock().unwrap() = tap as usize;
            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);
            let _ = tx.send(true);
            CFRunLoopRun();
        }
    })?;
    match rx.recv() {
        Ok(true) => {
            tracing::info!("watching the pointer for keyboard and mouse mode");
            Ok(())
        }
        _ => bail!("couldn't watch the keyboard and mouse: allow Freemonplay under Accessibility in System Settings"),
    }
}

extern "C" fn on_event(_proxy: *mut c_void, kind: u32, event: cg::CGEventRef, _info: *mut c_void) -> cg::CGEventRef {
    let Some(kvm) = KVM.get() else { return event };
    // SAFETY: reading fields of the event we were handed; the tap pointer is ours.
    unsafe {
        if kind == TAP_DISABLED_BY_TIMEOUT || kind == TAP_DISABLED_BY_USER_INPUT {
            let tap = *TAP.lock().unwrap() as CFMachPortRef;
            if !tap.is_null() {
                CGEventTapEnable(tap, true);
            }
            return event;
        }
        // Our own injected events (clients controlling this Mac's screens) aren't the
        // local keyboard and mouse.
        if CGEventGetIntegerValueField(event, FIELD_SOURCE_PID) == std::process::id() as i64 {
            return event;
        }
        if !kvm.is_active() {
            if matches!(kind, 5 | 6 | 7 | 27) {
                let at = CGEventGetLocation(event);
                let (dx, dy) = (CGEventGetDoubleValueField(event, FIELD_DELTA_X), CGEventGetDoubleValueField(event, FIELD_DELTA_Y));
                if let Some((display, bounds)) = display_at(at) {
                    let pointer = Pointer { display, bounds, x: at.x, y: at.y, dx, dy };
                    if kvm.check_entry(&pointer) {
                        MODIFIERS.store(device_modifiers(CGEventGetFlags(event)), Ordering::Relaxed);
                        return null_mut();
                    }
                }
            }
            return event;
        }

        // A client has the keyboard and mouse: forward, and swallow it here.
        let flags = CGEventGetFlags(event);
        let input = match kind {
            5 | 6 | 7 | 27 => Some(RemoteInput::Move {
                dx: CGEventGetDoubleValueField(event, FIELD_DELTA_X) as f32,
                dy: CGEventGetDoubleValueField(event, FIELD_DELTA_Y) as f32,
            }),
            1 | 2 => Some(RemoteInput::Button { button: MouseButton::Left, pressed: kind == 1 }),
            3 | 4 => Some(RemoteInput::Button { button: MouseButton::Right, pressed: kind == 3 }),
            25 | 26 => {
                let button = match CGEventGetIntegerValueField(event, FIELD_BUTTON_NUMBER) {
                    2 => MouseButton::Middle,
                    3 => MouseButton::Back,
                    4 => MouseButton::Forward,
                    _ => return null_mut(),
                };
                Some(RemoteInput::Button { button, pressed: kind == 25 })
            }
            SCROLL_WHEEL => Some(if CGEventGetIntegerValueField(event, FIELD_SCROLL_CONTINUOUS) != 0 {
                RemoteInput::Scroll {
                    dx: CGEventGetDoubleValueField(event, FIELD_SCROLL_POINTS_X) as f32,
                    dy: CGEventGetDoubleValueField(event, FIELD_SCROLL_POINTS_Y) as f32,
                    precise: true,
                }
            } else {
                RemoteInput::Scroll {
                    dx: CGEventGetIntegerValueField(event, FIELD_SCROLL_LINES_X) as f32,
                    dy: CGEventGetIntegerValueField(event, FIELD_SCROLL_LINES_Y) as f32,
                    precise: false,
                }
            }),
            KEY_DOWN | KEY_UP => {
                let code = CGEventGetIntegerValueField(event, FIELD_KEYCODE);
                let escape = cg::kCGEventFlagMaskControl | cg::kCGEventFlagMaskAlternate | cg::kCGEventFlagMaskShift;
                if kind == KEY_DOWN && code == ESCAPE_KEYCODE && flags & escape == escape {
                    kvm.release("Ctrl+Alt+Shift+K");
                    return null_mut();
                }
                // The client's OS repeats held keys itself.
                if CGEventGetIntegerValueField(event, FIELD_AUTOREPEAT) != 0 {
                    return null_mut();
                }
                hid_usage(code as u16).map(|usage| RemoteInput::Key { usage, pressed: kind == KEY_DOWN })
            }
            FLAGS_CHANGED => {
                for input in modifier_changes(CGEventGetIntegerValueField(event, FIELD_KEYCODE) as u16, flags) {
                    kvm.forward(input);
                }
                None
            }
            _ => None,
        };
        if let Some(input) = input {
            kvm.forward(input);
        }
        null_mut()
    }
}

/// The display under a point, and its bounds.
fn display_at(at: CGPoint) -> Option<(u32, Bounds)> {
    let (mut display, mut count) = (0u32, 0u32);
    // SAFETY: asks for at most one display id into our variable.
    let ok = unsafe { CGGetDisplaysWithPoint(at, 1, &mut display, &mut count) } == 0 && count == 1;
    ok.then(|| display_bounds(display)).flatten().map(|b| (display, b))
}

pub fn display_bounds(display: u32) -> Option<Bounds> {
    // SAFETY: a plain query.
    let r = unsafe { cg::CGDisplayBounds(display) };
    (r.size.width > 0.0).then_some(Bounds { x: r.origin.x, y: r.origin.y, width: r.size.width, height: r.size.height })
}

pub fn main_display() -> u32 {
    // SAFETY: a plain query.
    unsafe { CGMainDisplayID() }
}

/// Freezes the cursor at `park` and hides it while a client has the mouse.
pub fn capture_cursor(park: (f64, f64)) {
    // SAFETY: cursor calls with valid arguments; the private property lets a background
    // app hide the cursor (as other software KVMs do).
    unsafe {
        CGWarpMouseCursorPosition(CGPoint { x: park.0, y: park.1 });
        CGAssociateMouseAndMouseCursorPosition(false);
        allow_background_cursor_hiding();
        if !HIDDEN.swap(true, Ordering::SeqCst) {
            CGDisplayHideCursor(CGMainDisplayID());
        }
    }
}

/// Unfreezes the cursor, shows it and puts it at `at`.
pub fn release_cursor(at: (f64, f64)) {
    // SAFETY: as above.
    unsafe {
        CGWarpMouseCursorPosition(CGPoint { x: at.0, y: at.1 });
        CGAssociateMouseAndMouseCursorPosition(true);
        if HIDDEN.swap(false, Ordering::SeqCst) {
            CGDisplayShowCursor(CGMainDisplayID());
        }
    }
}

/// `CGSSetConnectionProperty(cid, cid, "SetsCursorInBackground", true)`, looked up at
/// run time since it's private.
unsafe fn allow_background_cursor_hiding() {
    type DefaultConnection = unsafe extern "C" fn() -> i32;
    type SetProperty = unsafe extern "C" fn(i32, i32, *const c_void, *const c_void) -> i32;
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    // SAFETY: looking up symbols in the already-loaded CoreGraphics; called with valid
    // CF objects if found.
    unsafe {
        let conn = libc_dlsym(c"_CGSDefaultConnection");
        let set = libc_dlsym(c"CGSSetConnectionProperty");
        if conn.is_null() || set.is_null() {
            tracing::debug!("can't hide the cursor in the background on this macOS");
            return;
        }
        let conn: DefaultConnection = std::mem::transmute(conn);
        let set: SetProperty = std::mem::transmute(set);
        let key = objc2_core_foundation::CFString::from_static_str("SetsCursorInBackground");
        let yes = objc2_core_foundation::CFBoolean::new(true);
        let cid = conn();
        set(cid, cid, &*key as *const _ as *const c_void, yes as *const _ as *const c_void);
    }
}

unsafe fn libc_dlsym(name: &std::ffi::CStr) -> *mut c_void {
    unsafe extern "C" {
        fn dlsym(handle: *mut c_void, symbol: *const std::ffi::c_char) -> *mut c_void;
    }
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    // SAFETY: dlsym with the default search handle and a valid C string.
    unsafe { dlsym(RTLD_DEFAULT, name.as_ptr()) }
}

/// Mac virtual keycode to USB HID usage (the inverse of input.rs's table).
fn hid_usage(code: u16) -> Option<u16> {
    static TABLE: OnceLock<[u16; 128]> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0u16; 128];
        for usage in 0..=0xE7u16 {
            if let Some(code) = fmp_proto::hid::mac_keycode(usage)
                && (code as usize) < table.len()
                && table[code as usize] == 0
            {
                table[code as usize] = usage;
            }
        }
        table
    });
    table.get(code as usize).copied().filter(|&u| u != 0)
}

// Device-dependent modifier flag bits (IOLLEvent.h), which tell left from right.
const DEVICE_MODIFIERS: [(u16, u64); 8] = [
    (0x3B, 0x0000_0001), // left control
    (0x38, 0x0000_0002), // left shift
    (0x3C, 0x0000_0004), // right shift
    (0x37, 0x0000_0008), // left command
    (0x36, 0x0000_0010), // right command
    (0x3A, 0x0000_0020), // left option
    (0x3D, 0x0000_0040), // right option
    (0x3E, 0x0000_2000), // right control
];
const CAPS_LOCK_KEYCODE: u16 = 0x39;

fn device_modifiers(flags: u64) -> u64 {
    DEVICE_MODIFIERS.iter().map(|&(_, bit)| bit).fold(0, |m, bit| m | (flags & bit))
}

/// Key presses and releases implied by a flags-changed event.
fn modifier_changes(code: u16, flags: u64) -> Vec<RemoteInput> {
    // Caps Lock reports its toggle state, not the key; send a full press each time.
    if code == CAPS_LOCK_KEYCODE {
        let Some(usage) = hid_usage(code) else { return Vec::new() };
        return vec![RemoteInput::Key { usage, pressed: true }, RemoteInput::Key { usage, pressed: false }];
    }
    let now = device_modifiers(flags);
    let before = MODIFIERS.swap(now, Ordering::Relaxed);
    DEVICE_MODIFIERS
        .iter()
        .filter(|&&(_, bit)| (now ^ before) & bit != 0)
        .filter_map(|&(key, bit)| Some(RemoteInput::Key { usage: hid_usage(key)?, pressed: now & bit != 0 }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmp_proto::hid;

    #[test]
    fn keycodes_map_back_to_hid() {
        assert_eq!(hid_usage(0x00), Some(hid::A));
        assert_eq!(hid_usage(0x24), Some(hid::ENTER));
        assert_eq!(hid_usage(0x37), Some(hid::META_LEFT));
        assert_eq!(hid_usage(0x7E), Some(hid::ARROW_UP));
    }
}
