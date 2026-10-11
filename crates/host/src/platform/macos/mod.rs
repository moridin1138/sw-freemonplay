mod capture;
mod cg;
mod input;
mod virtual_display;

pub use capture::Capture;
pub use input::Injector;
pub use virtual_display::VirtualDisplay;

use anyhow::{Result, bail};

/// Screen Recording is required for capture; Accessibility for input injection.
/// Both are granted to whatever app launched us (Terminal, iTerm, ...).
pub fn check_permissions() -> Result<()> {
    // SAFETY: plain CoreGraphics queries with no arguments.
    unsafe {
        if !cg::CGPreflightScreenCaptureAccess() {
            cg::CGRequestScreenCaptureAccess();
            bail!(
                "Screen Recording permission is needed. Enable it for your terminal app in \
                 System Settings > Privacy & Security > Screen & System Audio Recording, \
                 then restart the terminal and run this again."
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
