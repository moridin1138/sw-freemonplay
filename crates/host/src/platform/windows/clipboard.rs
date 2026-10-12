//! The Windows clipboard: text, or an image (PNG on the wire).

use std::borrow::Cow;

use arboard::ImageData;
use windows::Win32::System::DataExchange::{GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW};
use windows::core::w;

use crate::clipboard::Item;

pub fn change_count() -> i64 {
    // SAFETY: a plain query.
    unsafe { GetClipboardSequenceNumber() as i64 }
}

/// Text if there is any (most copies), otherwise an image; nothing if it's marked private.
pub fn read() -> Option<Item> {
    // Password managers put this format on copied secrets.
    // SAFETY: registering (or looking up) a named format, then a plain query.
    let private = unsafe {
        let format = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        format != 0 && IsClipboardFormatAvailable(format).is_ok()
    };
    if private {
        return None;
    }
    let mut clipboard = arboard::Clipboard::new().ok()?;
    if let Some(text) = clipboard.get_text().ok().filter(|t| !t.is_empty()) {
        return Some(Item::Text(text));
    }
    to_png(&clipboard.get_image().ok()?).map(Item::Image)
}

/// Puts an item on the clipboard; returns the new change count.
pub fn write(item: &Item) -> i64 {
    let result = arboard::Clipboard::new().and_then(|mut c| match item {
        Item::Text(text) => c.set_text(text),
        Item::Image(png) => match from_png(png) {
            Some(image) => c.set_image(image),
            None => {
                tracing::warn!("couldn't decode a copied image");
                Ok(())
            }
        },
    });
    if let Err(e) = result {
        tracing::warn!("couldn't set the clipboard: {e}");
    }
    change_count()
}

fn to_png(image: &ImageData) -> Option<Vec<u8>> {
    let mut png = Vec::new();
    let mut encoder = png::Encoder::new(&mut png, image.width as u32, image.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.write_header().ok()?.write_image_data(&image.bytes).ok()?;
    Some(png)
}

fn from_png(data: &[u8]) -> Option<ImageData<'static>> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
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
