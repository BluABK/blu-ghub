//! blu-ghub: a small replacement for Logitech G HUB, for one G915 keyboard
//! (on its LIGHTSPEED receiver) and one G935 headset.
//!
//! This first stage is diagnostics only. Nothing in it writes to a device:
//! [`diag::caps`] reads HID descriptors through handles opened with no access
//! rights, [`diag::sniff`] opens the HID++ collections read-only, and
//! [`diag::keys`] watches keyboard input through Raw Input, which observes
//! keystrokes without being in their path (no hook).

#[cfg(not(windows))]
compile_error!("blu-ghub talks to the Windows HID stack and only builds on Windows");

pub mod app;
pub mod app_paths;
pub mod battery;
pub mod config;
pub mod diag;
pub mod hid;
pub mod hidpp;
pub mod icon_art;
pub mod keynames;
pub mod platform;

// The build script's target-folder cleanup, compiled here only so its tests
// run with the rest of the suite.
#[cfg(test)]
#[path = "../build_support/target_gc.rs"]
mod target_gc;

/// Version string for logs and banners: crate version plus commit.
pub fn version() -> String {
    format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("GIT_HASH"))
}
