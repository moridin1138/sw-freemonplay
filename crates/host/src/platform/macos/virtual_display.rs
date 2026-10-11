//! Virtual monitors via CoreGraphics' private `CGVirtualDisplay` classes (macOS 11+).
//! These are what Sidecar, DeskPad and BetterDisplay use. Being private, they have no
//! headers, so we talk to them through the Objective-C runtime. Their shape:
//!
//! ```text
//! CGVirtualDisplayDescriptor: queue, name, maxPixelsWide/High (uint), sizeInMillimeters (CGSize),
//!                             vendorID, productID, serialNum (uint)
//! CGVirtualDisplayMode:       -initWithWidth:(uint)height:(uint)refreshRate:(double)
//! CGVirtualDisplaySettings:   modes (NSArray), hiDPI (uint)
//! CGVirtualDisplay:           -initWithDescriptor:, -applySettings: -> BOOL, displayID (uint)
//! ```

use anyhow::{Context, Result, bail};
use fmp_proto::DisplayRequest;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::msg_send;
use objc2_foundation::{NSArray, NSSize, NSString};

use super::cg;

unsafe extern "C" {
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut AnyObject;
}

/// Vendor id the OS will show for our displays. Arbitrary but fixed, so that together with a
/// per-client serial number macOS remembers where you arranged each client's screen.
const VENDOR_ID: u32 = 0xF4E0;
const PRODUCT_ID: u32 = 0x0001;

pub struct VirtualDisplay {
    // Keeping this alive keeps the monitor "plugged in".
    display: Retained<AnyObject>,
    id: u32,
}

// SAFETY: CGVirtualDisplay is only retained, read for its id and released; CoreGraphics
// objects are safe to retain/release from any thread.
unsafe impl Send for VirtualDisplay {}
unsafe impl Sync for VirtualDisplay {}

impl VirtualDisplay {
    pub fn create(name: &str, serial: u32, req: &DisplayRequest) -> Result<Self> {
        let class = |n: &std::ffi::CStr| {
            AnyClass::get(n).with_context(|| format!("{n:?} not found; needs macOS 11 or newer"))
        };

        // With HiDPI the OS renders 2x: a mode of W x H points uses 2W x 2H pixels.
        let (mode_w, mode_h) = if req.hidpi { (req.width / 2, req.height / 2) } else { (req.width, req.height) };
        let size_mm = req.size_mm.unwrap_or_else(|| {
            // Assume roughly 110 pixels per inch (about 4.3 per mm) of UI-sized screen.
            ((mode_w as f64 / 4.3) as u32, (mode_h as f64 / 4.3) as u32)
        });

        // SAFETY: selectors and argument types match the private headers quoted above. objc2
        // checks the type encodings in debug builds.
        unsafe {
            let desc: Retained<AnyObject> = msg_send![class(c"CGVirtualDisplayDescriptor")?, new];
            let queue = dispatch_get_global_queue(0, 0);
            let _: () = msg_send![&*desc, setQueue: queue];
            let _: () = msg_send![&*desc, setName: &*NSString::from_str(name)];
            let _: () = msg_send![&*desc, setMaxPixelsWide: req.width];
            let _: () = msg_send![&*desc, setMaxPixelsHigh: req.height];
            let _: () = msg_send![&*desc, setSizeInMillimeters: NSSize::new(size_mm.0 as f64, size_mm.1 as f64)];
            let _: () = msg_send![&*desc, setVendorID: VENDOR_ID];
            let _: () = msg_send![&*desc, setProductID: PRODUCT_ID];
            let _: () = msg_send![&*desc, setSerialNum: serial];

            let alloc: Allocated<AnyObject> = msg_send![class(c"CGVirtualDisplay")?, alloc];
            let display: Option<Retained<AnyObject>> = msg_send![alloc, initWithDescriptor: &*desc];
            let display = display.context("CGVirtualDisplay refused the descriptor")?;

            let alloc: Allocated<AnyObject> = msg_send![class(c"CGVirtualDisplayMode")?, alloc];
            let mode: Option<Retained<AnyObject>> =
                msg_send![alloc, initWithWidth: mode_w, height: mode_h, refreshRate: req.refresh_hz as f64];
            let mode = mode.context("could not create display mode")?;

            let settings: Retained<AnyObject> = msg_send![class(c"CGVirtualDisplaySettings")?, new];
            let modes = NSArray::from_retained_slice(&[mode]);
            let _: () = msg_send![&*settings, setModes: &*modes];
            let _: () = msg_send![&*settings, setHiDPI: req.hidpi as u32];
            let applied: bool = msg_send![&*display, applySettings: &*settings];
            if !applied {
                bail!("macOS rejected display mode {mode_w}x{mode_h}@{} (hidpi {})", req.refresh_hz, req.hidpi);
            }

            let id: u32 = msg_send![&*display, displayID];
            if id == 0 {
                bail!("virtual display has no id");
            }
            tracing::info!(
                "created virtual display {id} \"{name}\": {:?} pixels, {:?} points",
                cg::display_pixels(id),
                cg::CGDisplayBounds(id).size,
            );
            Ok(Self { display, id })
        }
    }

    pub fn id(&self) -> u32 {
        self.id
    }
}

impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        tracing::info!("removing virtual display {}", self.id);
        let _ = &self.display; // released right after this
    }
}
