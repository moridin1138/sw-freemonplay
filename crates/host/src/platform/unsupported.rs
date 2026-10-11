//! Placeholder for hosts that aren't implemented yet, so the workspace still builds there.
//!
//! Plan:
//! - Windows: bundle the MIT licensed Virtual Display Driver (an Indirect Display Driver)
//!   and drive it over its IOCTL/named-pipe interface; capture with
//!   Windows.Graphics.Capture; inject with SendInput.
//! - Linux: GNOME via Mutter's RemoteDesktop/ScreenCast D-Bus API (`RecordVirtual` makes a
//!   virtual monitor and gives a PipeWire stream), wlroots via `create_output`, X11 via a
//!   dummy xrandr output; capture with PipeWire; inject with libei or uinput.

use std::sync::Arc;

use anyhow::{Result, bail};
use fmp_proto::{DisplayRequest, InputEvent};

use crate::frame::FrameSlot;

pub fn hardware_encoder(
    _codec: fmp_proto::Codec,
    _w: u32,
    _h: u32,
    _fps: u32,
    _bitrate: u32,
) -> Result<Box<dyn crate::encoder::VideoEncoder>> {
    bail!("no hardware encoder support on this OS yet")
}

/// No-op where we don't have an equivalent yet.
pub struct Activity;

impl Activity {
    pub fn begin(_reason: &str) -> Self {
        Self
    }
}

pub fn prioritize_current_thread() {}

pub fn check_permissions() -> Result<()> {
    bail!("this OS can't host virtual displays yet (only macOS so far). The client works here though.")
}

pub struct VirtualDisplay;

impl VirtualDisplay {
    pub fn create(_name: &str, _serial: u32, _req: &DisplayRequest) -> Result<Self> {
        bail!("virtual displays are not implemented on this OS yet")
    }
}

pub struct Capture;

impl Capture {
    pub fn start(_display: &VirtualDisplay, _w: u32, _h: u32, _fps: u32, _slot: Arc<FrameSlot>) -> Result<Self> {
        bail!("capture is not implemented on this OS yet")
    }
}

pub struct Injector;

impl Injector {
    pub fn new(_display: &VirtualDisplay) -> Self {
        Self
    }
    pub fn inject(&mut self, _event: &InputEvent) {}
}
