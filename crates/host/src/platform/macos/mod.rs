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
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

/// While alive, tells macOS that latency-critical work is under way: no App Nap (which
/// throttles Dock-less apps like ours), and no idle display sleep (which would blank the
/// virtual displays when nobody touches the Mac itself).
pub struct Activity(Retained<ProtocolObject<dyn NSObjectProtocol>>);

// SAFETY: the activity token is an immutable object only passed back to NSProcessInfo,
// which is thread-safe.
unsafe impl Send for Activity {}
unsafe impl Sync for Activity {}

impl Activity {
    pub fn begin(reason: &str) -> Self {
        let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
            | NSActivityOptions::IdleDisplaySleepDisabled
            | NSActivityOptions::LatencyCritical;
        let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &NSString::from_str(reason));
        Self(token)
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        // SAFETY: ending the activity we began.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
    }
}

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
}

/// Runs the calling thread at the highest QoS, so it gets performance cores promptly.
pub fn prioritize_current_thread() {
    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    // SAFETY: changes only the calling thread's scheduling class.
    unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) };
}

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
