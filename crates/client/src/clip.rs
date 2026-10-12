//! Clipboard sharing with the host, client side: text copied here goes to the host, text
//! copied there arrives here. Polled about once a second; the last text sent or received
//! is remembered so nothing bounces back. Items that password managers mark as private
//! are never sent.

use std::sync::Mutex;

pub struct ClipSync {
    /// The text both sides have, as far as we know.
    last: Mutex<Option<String>>,
    /// The OS's clipboard change counter when we last looked (Windows, macOS).
    seen: Mutex<Option<u64>>,
}

impl ClipSync {
    pub fn new() -> Self {
        Self { last: Mutex::new(None), seen: Mutex::new(imp::change_count()) }
    }

    /// New text copied here since we last looked, if any.
    pub fn copied(&self) -> Option<String> {
        let count = imp::change_count();
        {
            let mut seen = self.seen.lock().unwrap();
            if count.is_some() && *seen == count {
                return None;
            }
            *seen = count;
        }
        let text = imp::text()?;
        let mut last = self.last.lock().unwrap();
        if last.as_deref() == Some(text.as_str()) {
            return None;
        }
        *last = Some(text.clone());
        Some(text)
    }

    /// Text copied on the host.
    pub fn received(&self, text: String) {
        imp::set_text(&text);
        *self.seen.lock().unwrap() = imp::change_count();
        *self.last.lock().unwrap() = Some(text);
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use windows::Win32::System::DataExchange::{
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    use windows::core::w;

    pub fn change_count() -> Option<u64> {
        // SAFETY: a plain query.
        Some(unsafe { GetClipboardSequenceNumber() } as u64)
    }

    pub fn text() -> Option<String> {
        // Password managers put this format on copied secrets (Windows clipboard history
        // and cloud sync skip them too).
        // SAFETY: registering (or looking up) a named format, then a plain query.
        let private = unsafe {
            let format = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
            format != 0 && IsClipboardFormatAvailable(format).is_ok()
        };
        if private {
            return None;
        }
        arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
    }

    pub fn set_text(text: &str) {
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            tracing::warn!("couldn't set the clipboard: {e}");
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    /// Markers password managers and similar apps put on private items (nspasteboard.org).
    const PRIVATE_TYPES: [&str; 2] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

    pub fn change_count() -> Option<u64> {
        Some(NSPasteboard::generalPasteboard().changeCount() as u64)
    }

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

    pub fn set_text(text: &str) {
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        // SAFETY: framework constant.
        board.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    //! X11 and Wayland have no change counter here, so the text itself is compared.

    pub fn change_count() -> Option<u64> {
        None
    }

    pub fn text() -> Option<String> {
        arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
    }

    pub fn set_text(text: &str) {
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
            tracing::warn!("couldn't set the clipboard: {e}");
        }
    }
}
