//! USB HID keyboard usage ids (usage page 0x07). These are layout independent physical
//! key positions, which is what both sides need: the client reports which key was hit,
//! and the host's own keyboard layout decides what character it produces.

pub const A: u16 = 0x04; // A..Z are consecutive up to 0x1D
pub const Z: u16 = 0x1D;
pub const DIGIT1: u16 = 0x1E; // 1..9 are consecutive, then 0
pub const DIGIT0: u16 = 0x27;
pub const ENTER: u16 = 0x28;
pub const ESCAPE: u16 = 0x29;
pub const BACKSPACE: u16 = 0x2A;
pub const TAB: u16 = 0x2B;
pub const SPACE: u16 = 0x2C;
pub const MINUS: u16 = 0x2D;
pub const EQUAL: u16 = 0x2E;
pub const BRACKET_LEFT: u16 = 0x2F;
pub const BRACKET_RIGHT: u16 = 0x30;
pub const BACKSLASH: u16 = 0x31;
pub const SEMICOLON: u16 = 0x33;
pub const QUOTE: u16 = 0x34;
pub const BACKQUOTE: u16 = 0x35;
pub const COMMA: u16 = 0x36;
pub const PERIOD: u16 = 0x37;
pub const SLASH: u16 = 0x38;
pub const CAPS_LOCK: u16 = 0x39;
pub const F1: u16 = 0x3A; // F1..F12 are consecutive
pub const F12: u16 = 0x45;
pub const PRINT_SCREEN: u16 = 0x46;
pub const SCROLL_LOCK: u16 = 0x47;
pub const PAUSE: u16 = 0x48;
pub const INSERT: u16 = 0x49;
pub const HOME: u16 = 0x4A;
pub const PAGE_UP: u16 = 0x4B;
pub const DELETE: u16 = 0x4C;
pub const END: u16 = 0x4D;
pub const PAGE_DOWN: u16 = 0x4E;
pub const ARROW_RIGHT: u16 = 0x4F;
pub const ARROW_LEFT: u16 = 0x50;
pub const ARROW_DOWN: u16 = 0x51;
pub const ARROW_UP: u16 = 0x52;
pub const NUM_LOCK: u16 = 0x53;
pub const NUMPAD_DIVIDE: u16 = 0x54;
pub const NUMPAD_MULTIPLY: u16 = 0x55;
pub const NUMPAD_SUBTRACT: u16 = 0x56;
pub const NUMPAD_ADD: u16 = 0x57;
pub const NUMPAD_ENTER: u16 = 0x58;
pub const NUMPAD1: u16 = 0x59; // 1..9 are consecutive, then 0
pub const NUMPAD0: u16 = 0x62;
pub const NUMPAD_DECIMAL: u16 = 0x63;
pub const INTL_BACKSLASH: u16 = 0x64;
pub const CONTEXT_MENU: u16 = 0x65;
pub const NUMPAD_EQUAL: u16 = 0x67;
pub const F13: u16 = 0x68; // F13..F24 are consecutive
pub const F24: u16 = 0x73;
pub const CONTROL_LEFT: u16 = 0xE0;
pub const SHIFT_LEFT: u16 = 0xE1;
pub const ALT_LEFT: u16 = 0xE2;
pub const META_LEFT: u16 = 0xE3;
pub const CONTROL_RIGHT: u16 = 0xE4;
pub const SHIFT_RIGHT: u16 = 0xE5;
pub const ALT_RIGHT: u16 = 0xE6;
pub const META_RIGHT: u16 = 0xE7;

/// HID usage to macOS virtual key code (kVK_* from HIToolbox/Events.h).
pub fn mac_keycode(usage: u16) -> Option<u16> {
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
        A..=Z => LETTERS[(usage - A) as usize],
        DIGIT1..=DIGIT0 => DIGITS[(usage - DIGIT1) as usize],
        F1..=F12 => FKEYS[(usage - F1) as usize],
        F13..=F24 => *FKEYS_HIGH.get((usage - F13) as usize)?,
        NUMPAD1..=NUMPAD0 => NUMPAD[(usage - NUMPAD1) as usize],
        ENTER => 0x24,
        ESCAPE => 0x35,
        BACKSPACE => 0x33,
        TAB => 0x30,
        SPACE => 0x31,
        MINUS => 0x1B,
        EQUAL => 0x18,
        BRACKET_LEFT => 0x21,
        BRACKET_RIGHT => 0x1E,
        BACKSLASH => 0x2A,
        SEMICOLON => 0x29,
        QUOTE => 0x27,
        BACKQUOTE => 0x32,
        COMMA => 0x2B,
        PERIOD => 0x2F,
        SLASH => 0x2C,
        CAPS_LOCK => 0x39,
        INSERT => 0x72, // Help
        HOME => 0x73,
        PAGE_UP => 0x74,
        DELETE => 0x75,
        END => 0x77,
        PAGE_DOWN => 0x79,
        ARROW_RIGHT => 0x7C,
        ARROW_LEFT => 0x7B,
        ARROW_DOWN => 0x7D,
        ARROW_UP => 0x7E,
        NUM_LOCK => 0x47, // Clear
        NUMPAD_DIVIDE => 0x4B,
        NUMPAD_MULTIPLY => 0x43,
        NUMPAD_SUBTRACT => 0x4E,
        NUMPAD_ADD => 0x45,
        NUMPAD_ENTER => 0x4C,
        NUMPAD_DECIMAL => 0x41,
        NUMPAD_EQUAL => 0x51,
        INTL_BACKSLASH => 0x0A,
        CONTEXT_MENU => 0x6E,
        CONTROL_LEFT => 0x3B,
        SHIFT_LEFT => 0x38,
        ALT_LEFT => 0x3A,
        META_LEFT => 0x37,
        CONTROL_RIGHT => 0x3E,
        SHIFT_RIGHT => 0x3C,
        ALT_RIGHT => 0x3D,
        META_RIGHT => 0x36,
        _ => return None,
    })
}

/// USB HID usage to a set 1 scancode, and whether it's an extended (E0) key.
pub fn set1_scancode(usage: u16) -> Option<(u16, bool)> {
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
        A..=Z => plain(LETTERS[(usage - A) as usize]),
        // 1..9 are 0x02..0x0A, 0 is 0x0B
        DIGIT1..=DIGIT0 => plain(0x02 + usage - DIGIT1),
        F1..=F12 => plain(FKEYS[(usage - F1) as usize]),
        F13..=F24 => plain(FKEYS_HIGH[(usage - F13) as usize]),
        NUMPAD1..=NUMPAD0 => plain(NUMPAD[(usage - NUMPAD1) as usize]),
        ENTER => plain(0x1C),
        ESCAPE => plain(0x01),
        BACKSPACE => plain(0x0E),
        TAB => plain(0x0F),
        SPACE => plain(0x39),
        MINUS => plain(0x0C),
        EQUAL => plain(0x0D),
        BRACKET_LEFT => plain(0x1A),
        BRACKET_RIGHT => plain(0x1B),
        BACKSLASH => plain(0x2B),
        SEMICOLON => plain(0x27),
        QUOTE => plain(0x28),
        BACKQUOTE => plain(0x29),
        COMMA => plain(0x33),
        PERIOD => plain(0x34),
        SLASH => plain(0x35),
        CAPS_LOCK => plain(0x3A),
        PRINT_SCREEN => ext(0x37),
        SCROLL_LOCK => plain(0x46),
        INSERT => ext(0x52),
        HOME => ext(0x47),
        PAGE_UP => ext(0x49),
        DELETE => ext(0x53),
        END => ext(0x4F),
        PAGE_DOWN => ext(0x51),
        ARROW_RIGHT => ext(0x4D),
        ARROW_LEFT => ext(0x4B),
        ARROW_DOWN => ext(0x50),
        ARROW_UP => ext(0x48),
        NUM_LOCK => plain(0x45),
        NUMPAD_DIVIDE => ext(0x35),
        NUMPAD_MULTIPLY => plain(0x37),
        NUMPAD_SUBTRACT => plain(0x4A),
        NUMPAD_ADD => plain(0x4E),
        NUMPAD_ENTER => ext(0x1C),
        NUMPAD_DECIMAL => plain(0x53),
        INTL_BACKSLASH => plain(0x56),
        CONTEXT_MENU => ext(0x5D),
        CONTROL_LEFT => plain(0x1D),
        SHIFT_LEFT => plain(0x2A),
        ALT_LEFT => plain(0x38),
        META_LEFT => ext(0x5B),
        CONTROL_RIGHT => ext(0x1D),
        SHIFT_RIGHT => plain(0x36),
        ALT_RIGHT => ext(0x38),
        META_RIGHT => ext(0x5C),
        _ => None,
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_keys() {
        assert_eq!(mac_keycode(A), Some(0x00));
        assert_eq!(mac_keycode(ARROW_UP), Some(0x7E));
        assert_eq!(set1_scancode(A), Some((0x1E, false)));
        assert_eq!(set1_scancode(DIGIT0), Some((0x0B, false)));
        assert_eq!(set1_scancode(ARROW_UP), Some((0x48, true)));
        assert_eq!(set1_scancode(F24), Some((0x76, false)));
    }
}
