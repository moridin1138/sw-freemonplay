//! Per-OS pieces. Every platform module exposes the same three types:
//!
//! - `VirtualDisplay`: an extra monitor the OS believes is plugged in. Dropping it unplugs it.
//! - `Capture`: streams frames of that display into a [`FrameSlot`]. Dropping it stops capture.
//! - `Injector`: turns client [`InputEvent`]s into OS mouse/keyboard events on that display.
//!
//! plus `check_permissions()`, called once at startup.
//!
//! [`FrameSlot`]: crate::frame::FrameSlot
//! [`InputEvent`]: fmp_proto::InputEvent

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(not(target_os = "macos"))]
pub use unsupported::*;
