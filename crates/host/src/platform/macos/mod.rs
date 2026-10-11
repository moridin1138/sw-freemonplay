mod capture;
mod cg;
mod input;
mod videotoolbox;
mod virtual_display;

pub use capture::{Capture, NativeFrame};
pub use videotoolbox::hardware_encoder;
pub use input::Injector;
pub use virtual_display::VirtualDisplay;

use anyhow::Result;

/// Screen Recording is required for capture; Accessibility for input injection.
/// Both are granted to whatever app launched us (Terminal, iTerm, ...).
///
/// Only warns: the preflight check can report "denied" in setups where capture works
/// anyway, so the real signal is whether frames arrive (each session warns if they don't).
pub fn check_permissions() -> Result<()> {
    // SAFETY: plain CoreGraphics queries with no arguments.
    unsafe {
        if !cg::CGPreflightScreenCaptureAccess() {
            cg::CGRequestScreenCaptureAccess();
            tracing::warn!(
                "macOS reports no Screen Recording permission. If clients stay black, enable it for \
                 your terminal app in System Settings > Privacy & Security > Screen & System Audio \
                 Recording, then restart the terminal."
            );
        }
        if !cg::CGPreflightPostEventAccess() {
            cg::CGRequestPostEventAccess();
            tracing::warn!(
                "Accessibility permission is missing, so input from clients will be ignored. \
                 Enable it for your terminal app in System Settings > Privacy & Security > Accessibility."
            );
        }
    }
    Ok(())
}
