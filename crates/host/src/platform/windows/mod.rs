//! Windows host.
//!
//! Each screen gets a virtual monitor from the Virtual Display Driver (display.rs; the
//! primary monitor is shared if the driver isn't installed): Windows.Graphics.Capture,
//! converted to NV12 on the GPU, hardware encoded by Media Foundation on the same device (no
//! copies through system memory), with input injected by SendInput.

mod capture;
pub mod clipboard;
mod d3d;
pub mod display;
pub mod driver;
mod encoder;
mod input;
pub mod system;

use anyhow::Result;
use windows::Win32::System::Com::CoIncrementMTAUsage;
use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};

pub use capture::{Capture, NativeFrame};
pub use display::VirtualDisplay;
pub use input::Injector;

pub fn hardware_encoder(
    codec: fmp_proto::Codec,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
) -> Result<Box<dyn crate::encoder::VideoEncoder>> {
    Ok(Box::new(encoder::MfEncoder::new(codec, width, height, fps, bitrate)?))
}

/// Keeps the PC and its display awake while screens are connected.
pub struct Activity;

impl Activity {
    pub fn begin(_reason: &str) -> Self {
        // SAFETY: changes only this thread's power request. Held by the session registry's
        // thread for as long as anything is connected.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED) };
        Self
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: as above.
        unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
    }
}

pub fn prioritize_current_thread() {
    // SAFETY: raising our own thread's priority.
    let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
}

/// Nothing to ask for on Windows. Monitor coordinates should be real pixels, though.
pub fn check_permissions() -> Result<()> {
    // SAFETY: process-wide settings, made once at startup.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        // Keep COM's multithreaded apartment alive for the whole run. Capture and encoding
        // are set up on short-lived worker threads; when the last of those left the
        // apartment, Windows unloaded the capture library while capture was still running
        // (a crash about 10 s into a session, when the worker retired).
        if let Err(e) = CoIncrementMTAUsage() {
            tracing::warn!("couldn't pin COM's multithreaded apartment: {e}");
        }
    }
    display::hide_leftovers();
    Ok(())
}

pub mod kvm {
    //! Keyboard and mouse mode with Windows as the host needs low-level input hooks; later.
    use std::sync::Arc;

    use anyhow::{Result, bail};

    use crate::kvm::{Bounds, Kvm};

    pub fn start(_kvm: Arc<Kvm>) -> Result<()> {
        bail!("keyboard and mouse mode isn't available on Windows hosts yet")
    }
    pub fn display_bounds(_display: u32) -> Option<Bounds> {
        None
    }
    pub fn main_display() -> u32 {
        0
    }
    pub fn capture_cursor(_park: (f64, f64)) {}
    pub fn release_cursor(_at: (f64, f64)) {}
}

pub mod network {
    use fmp_proto::NetworkInfo;

    /// This PC's Wi-Fi link (not inspected yet).
    pub fn describe() -> Option<NetworkInfo> {
        None
    }
}
