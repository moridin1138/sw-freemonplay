//! Converts winit input into protocol events.

use fmp_proto::{MouseButton, hid};
use winit::event::MouseButton as WinitButton;
use winit::keyboard::KeyCode;

pub fn button(b: WinitButton) -> Option<MouseButton> {
    Some(match b {
        WinitButton::Left => MouseButton::Left,
        WinitButton::Right => MouseButton::Right,
        WinitButton::Middle => MouseButton::Middle,
        WinitButton::Back => MouseButton::Back,
        WinitButton::Forward => MouseButton::Forward,
        WinitButton::Other(_) => return None,
    })
}

/// Maps a window position (physical pixels) to 0..1 coordinates on the letterboxed video.
pub fn normalize(pos: (f64, f64), video: (u32, u32), window: (u32, u32)) -> (f32, f32) {
    let (sx, sy) = crate::render::letterbox_scale(video, window);
    let (ww, wh) = (window.0 as f64, window.1 as f64);
    let (vw, vh) = (ww * sx as f64, wh * sy as f64);
    let (ox, oy) = ((ww - vw) / 2.0, (wh - vh) / 2.0);
    (((pos.0 - ox) / vw).clamp(0.0, 1.0) as f32, ((pos.1 - oy) / vh).clamp(0.0, 1.0) as f32)
}

/// Physical key to USB HID usage id.
pub fn hid_usage(code: KeyCode) -> Option<u16> {
    use KeyCode::*;
    let letters = [
        KeyA, KeyB, KeyC, KeyD, KeyE, KeyF, KeyG, KeyH, KeyI, KeyJ, KeyK, KeyL, KeyM, KeyN, KeyO, KeyP, KeyQ, KeyR,
        KeyS, KeyT, KeyU, KeyV, KeyW, KeyX, KeyY, KeyZ,
    ];
    let digits = [Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9, Digit0];
    let fkeys = [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20, F21, F22, F23, F24,
    ];
    let numpad = [Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9, Numpad0];

    if let Some(i) = letters.iter().position(|&k| k == code) {
        return Some(hid::A + i as u16);
    }
    if let Some(i) = digits.iter().position(|&k| k == code) {
        return Some(hid::DIGIT1 + i as u16);
    }
    if let Some(i) = fkeys.iter().position(|&k| k == code) {
        return Some(if i < 12 { hid::F1 + i as u16 } else { hid::F13 + (i - 12) as u16 });
    }
    if let Some(i) = numpad.iter().position(|&k| k == code) {
        return Some(hid::NUMPAD1 + i as u16);
    }
    Some(match code {
        Enter => hid::ENTER,
        Escape => hid::ESCAPE,
        Backspace => hid::BACKSPACE,
        Tab => hid::TAB,
        Space => hid::SPACE,
        Minus => hid::MINUS,
        Equal => hid::EQUAL,
        BracketLeft => hid::BRACKET_LEFT,
        BracketRight => hid::BRACKET_RIGHT,
        Backslash => hid::BACKSLASH,
        Semicolon => hid::SEMICOLON,
        Quote => hid::QUOTE,
        Backquote => hid::BACKQUOTE,
        Comma => hid::COMMA,
        Period => hid::PERIOD,
        Slash => hid::SLASH,
        CapsLock => hid::CAPS_LOCK,
        PrintScreen => hid::PRINT_SCREEN,
        ScrollLock => hid::SCROLL_LOCK,
        Pause => hid::PAUSE,
        Insert => hid::INSERT,
        Home => hid::HOME,
        PageUp => hid::PAGE_UP,
        Delete => hid::DELETE,
        End => hid::END,
        PageDown => hid::PAGE_DOWN,
        ArrowRight => hid::ARROW_RIGHT,
        ArrowLeft => hid::ARROW_LEFT,
        ArrowDown => hid::ARROW_DOWN,
        ArrowUp => hid::ARROW_UP,
        NumLock => hid::NUM_LOCK,
        NumpadDivide => hid::NUMPAD_DIVIDE,
        NumpadMultiply => hid::NUMPAD_MULTIPLY,
        NumpadSubtract => hid::NUMPAD_SUBTRACT,
        NumpadAdd => hid::NUMPAD_ADD,
        NumpadEnter => hid::NUMPAD_ENTER,
        NumpadDecimal => hid::NUMPAD_DECIMAL,
        NumpadEqual => hid::NUMPAD_EQUAL,
        IntlBackslash => hid::INTL_BACKSLASH,
        ContextMenu => hid::CONTEXT_MENU,
        ControlLeft => hid::CONTROL_LEFT,
        ShiftLeft => hid::SHIFT_LEFT,
        AltLeft => hid::ALT_LEFT,
        SuperLeft => hid::META_LEFT,
        ControlRight => hid::CONTROL_RIGHT,
        ShiftRight => hid::SHIFT_RIGHT,
        AltRight => hid::ALT_RIGHT,
        SuperRight => hid::META_RIGHT,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys() {
        assert_eq!(hid_usage(KeyCode::KeyA), Some(hid::A));
        assert_eq!(hid_usage(KeyCode::KeyZ), Some(hid::Z));
        assert_eq!(hid_usage(KeyCode::Digit0), Some(hid::DIGIT0));
        assert_eq!(hid_usage(KeyCode::F12), Some(hid::F12));
        assert_eq!(hid_usage(KeyCode::F13), Some(hid::F13));
        assert_eq!(hid_usage(KeyCode::Numpad0), Some(hid::NUMPAD0));
    }

    #[test]
    fn normalizes_through_letterbox() {
        // 16:9 video in a 4:3 window: bars top and bottom.
        let (x, y) = normalize((400.0, 300.0), (1920, 1080), (800, 600));
        assert!((x - 0.5).abs() < 1e-6 && (y - 0.5).abs() < 1e-6);
        let (_, top) = normalize((0.0, 75.0), (1920, 1080), (800, 600));
        assert!(top.abs() < 1e-6);
    }
}
