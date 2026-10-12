//! The general pasteboard: text, or an image (as PNG).

use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString,
    NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSDictionary, NSString};

use crate::clipboard::Item;

/// Markers that password managers and similar apps put on items not meant to be shared
/// (nspasteboard.org).
const PRIVATE_TYPES: [&str; 2] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

pub fn change_count() -> i64 {
    NSPasteboard::generalPasteboard().changeCount() as i64
}

/// Text if there is any (most copies), otherwise an image; nothing if it's marked private.
pub fn read() -> Option<Item> {
    let board = NSPasteboard::generalPasteboard();
    if let Some(types) = board.types()
        && types.iter().any(|t| PRIVATE_TYPES.contains(&t.to_string().as_str()))
    {
        return None;
    }
    // SAFETY: framework constants.
    let (string, png, tiff) = unsafe { (NSPasteboardTypeString, NSPasteboardTypePNG, NSPasteboardTypeTIFF) };
    if let Some(text) = board.stringForType(string).map(|s| s.to_string()).filter(|t| !t.is_empty()) {
        return Some(Item::Text(text));
    }
    if let Some(data) = board.dataForType(png) {
        return Some(Item::Image(data.to_vec()));
    }
    // Screenshots and Preview copies are often TIFF only.
    let data = board.dataForType(tiff)?;
    let rep = NSBitmapImageRep::imageRepWithData(&data)?;
    // SAFETY: an empty properties dictionary is valid.
    let png = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }?;
    Some(Item::Image(png.to_vec()))
}

/// Puts an item on the clipboard; returns the new change count.
pub fn write(item: &Item) -> i64 {
    let board = NSPasteboard::generalPasteboard();
    board.clearContents();
    // SAFETY: framework constants.
    unsafe {
        match item {
            Item::Text(text) => board.setString_forType(&NSString::from_str(text), NSPasteboardTypeString),
            Item::Image(png) => board.setData_forType(Some(&NSData::with_bytes(png)), NSPasteboardTypePNG),
        };
    }
    board.changeCount() as i64
}
