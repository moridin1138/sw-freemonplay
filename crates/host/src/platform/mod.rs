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

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use unsupported::*;
