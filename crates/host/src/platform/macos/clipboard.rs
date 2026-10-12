//! The general pasteboard, as plain text.

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::NSString;

/// Markers that password managers and similar apps put on items not meant to be shared
/// (nspasteboard.org).
const PRIVATE_TYPES: [&str; 2] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

pub fn change_count() -> i64 {
    NSPasteboard::generalPasteboard().changeCount() as i64
}

/// The text on the clipboard, unless there's none or it's marked private.
pub fn text() -> Option<String> {
    let board = NSPasteboard::generalPasteboard();
    if let Some(types) = board.types()
        && types.iter().any(|t| PRIVATE_TYPES.contains(&t.to_string().as_str()))
    {
        return None;
    }
    // SAFETY: framework constant.
    let text = board.stringForType(unsafe { NSPasteboardTypeString })?.to_string();
    (!text.is_empty()).then_some(text)
}

/// Puts text on the clipboard; returns the new change count.
pub fn set_text(text: &str) -> i64 {
    let board = NSPasteboard::generalPasteboard();
    board.clearContents();
    // SAFETY: framework constant.
    board.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    board.changeCount() as i64
}
