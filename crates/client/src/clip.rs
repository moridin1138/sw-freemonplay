//! Clipboard sharing with the host, client side: what's copied here (text, or an image as
//! PNG) goes to the host, and what's copied there arrives here. Polled about once a second;
//! the last item sent or received is remembered so nothing bounces back. Items that
//! password managers mark as private are never sent.

use std::hash::{Hash, Hasher};
use std::sync::Mutex;

use fmp_proto::{ClientMsg, MAX_CLIPBOARD_IMAGE_BYTES};

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Text(String),
    /// PNG.
    Image(Vec<u8>),
}

impl Item {
    fn fingerprint(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.hash(&mut h);
        h.finish()
    }

    pub fn into_message(self) -> ClientMsg {
        match self {
            Item::Text(text) => ClientMsg::Clipboard(text),
            Item::Image(png) => ClientMsg::ClipboardImage(png),
        }
    }
}

pub struct ClipSync {
    /// The item both sides have, as far as we know.
    last: Mutex<Option<u64>>,
    /// The OS's clipboard change counter when we last looked (Windows, macOS).
    seen: Mutex<Option<u64>>,
}

impl ClipSync {
    pub fn new() -> Self {
        Self { last: Mutex::new(None), seen: Mutex::new(imp::change_count()) }
    }

    /// Something newly copied here, if any. Can take a moment for big images (PNG encoding),
    /// so call it off the async threads.
    pub fn copied(&self) -> Option<Item> {
        let count = imp::change_count();
        {
            let mut seen = self.seen.lock().unwrap();
            if count.is_some() && *seen == count {
                return None;
            }
            *seen = count;
        }
        let item = imp::read()?;
        if let Item::Image(png) = &item
            && png.len() > MAX_CLIPBOARD_IMAGE_BYTES
        {
            tracing::info!("not sharing a {} MB copied image (too big)", png.len() / 1_000_000);
            return None;
        }
        let fingerprint = item.fingerprint();
        let mut last = self.last.lock().unwrap();
        if *last == Some(fingerprint) {
            return None;
        }
        *last = Some(fingerprint);
        Some(item)
    }

    /// Something copied on the host.
    pub fn received(&self, item: Item) {
        let fingerprint = item.fingerprint();
        imp::write(&item);
        *self.seen.lock().unwrap() = imp::change_count();
        *self.last.lock().unwrap() = Some(fingerprint);
    }
}

#[cfg(not(target_os = "macos"))]
mod pixels {
    //! arboard hands images over as RGBA pixels; the protocol carries PNG.
    use std::borrow::Cow;

    use arboard::ImageData;

    pub fn to_png(image: &ImageData) -> Option<Vec<u8>> {
        let mut png = Vec::new();
        let mut encoder = png::Encoder::new(&mut png, image.width as u32, image.height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        // Fast rather than small: the network is quicker than heavy compression.
        encoder.set_compression(png::Compression::Fast);
        encoder.write_header().ok()?.write_image_data(&image.bytes).ok()?;
        Some(png)
    }

    pub fn from_png(data: &[u8]) -> Option<ImageData<'static>> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
        // Expand palettes, grey and 16 bit images to 8 bit RGB(A).
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        let pixels = &buf[..info.buffer_size()];
        let rgba = match info.color_type {
            png::ColorType::Rgba => pixels.to_vec(),
            png::ColorType::Rgb => pixels.as_chunks::<3>().0.iter().flat_map(|&[r, g, b]| [r, g, b, 255]).collect(),
            png::ColorType::GrayscaleAlpha => pixels.as_chunks::<2>().0.iter().flat_map(|&[g, a]| [g, g, g, a]).collect(),
            png::ColorType::Grayscale => pixels.iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => return None,
        };
        Some(ImageData { width: info.width as usize, height: info.height as usize, bytes: Cow::Owned(rgba) })
    }

    pub fn read_image() -> Option<Vec<u8>> {
        to_png(&arboard::Clipboard::new().ok()?.get_image().ok()?)
    }

    pub fn write_image(png: &[u8]) {
        let Some(image) = from_png(png) else {
            tracing::warn!("couldn't decode a copied image from the host");
            return;
        };
        if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_image(image)) {
            tracing::warn!("couldn't put an image on the clipboard: {e}");
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn png_round_trip() {
            let image = ImageData { width: 2, height: 1, bytes: Cow::Owned(vec![255, 0, 0, 255, 0, 0, 255, 128]) };
            let back = from_png(&to_png(&image).unwrap()).unwrap();
            assert_eq!((back.width, back.height), (2, 1));
            assert_eq!(back.bytes, image.bytes);
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use windows::Win32::System::DataExchange::{
        GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    use windows::core::w;

    use super::{Item, pixels};

    pub fn change_count() -> Option<u64> {
        // SAFETY: a plain query.
        Some(unsafe { GetClipboardSequenceNumber() } as u64)
    }

    /// Text if there is any (most copies), otherwise an image.
    pub fn read() -> Option<Item> {
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
        if let Some(text) = arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty()) {
            return Some(Item::Text(text));
        }
        pixels::read_image().map(Item::Image)
    }

    pub fn write(item: &Item) {
        match item {
            Item::Text(text) => {
                if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
                    tracing::warn!("couldn't set the clipboard: {e}");
                }
            }
            Item::Image(png) => pixels::write_image(png),
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypePNG, NSPasteboardTypeString,
        NSPasteboardTypeTIFF,
    };
    use objc2_foundation::{NSData, NSDictionary, NSString};

    use super::Item;

    /// Markers password managers and similar apps put on private items (nspasteboard.org).
    const PRIVATE_TYPES: [&str; 2] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

    pub fn change_count() -> Option<u64> {
        Some(NSPasteboard::generalPasteboard().changeCount() as u64)
    }

    /// Text if there is any (most copies), otherwise an image.
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

    pub fn write(item: &Item) {
        let board = NSPasteboard::generalPasteboard();
        board.clearContents();
        // SAFETY: framework constants.
        unsafe {
            match item {
                Item::Text(text) => board.setString_forType(&NSString::from_str(text), NSPasteboardTypeString),
                Item::Image(png) => board.setData_forType(Some(&NSData::with_bytes(png)), NSPasteboardTypePNG),
            };
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod imp {
    //! X11 and Wayland have no change counter here, so the content itself is compared.
    use super::{Item, pixels};

    pub fn change_count() -> Option<u64> {
        None
    }

    pub fn read() -> Option<Item> {
        if let Some(text) = arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty()) {
            return Some(Item::Text(text));
        }
        pixels::read_image().map(Item::Image)
    }

    pub fn write(item: &Item) {
        match item {
            Item::Text(text) => {
                if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
                    tracing::warn!("couldn't set the clipboard: {e}");
                }
            }
            Item::Image(png) => pixels::write_image(png),
        }
    }
}
