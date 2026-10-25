//! blu-ghub: a small replacement for Logitech G HUB, for one G915 keyboard
//! (on its LIGHTSPEED receiver or its cable) and one G935 headset.
//!
//! - [`app`]: the tray app (BluGHub.exe). Battery levels so far, read with
//!   HID++ requests only when a device connects or wakes, when asked, and as a
//!   30-minute fallback while it is awake; the only write is turning on the
//!   receiver's own connection notifications.
//! - [`diag`]: BluGHub-diag. `caps` reads HID descriptors through handles
//!   opened with no access rights, `sniff` opens the HID++ collections
//!   read-only, `keys` watches keyboard input through Raw Input (no hook), and
//!   `probe`/`dump-onboard` send read requests only.

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
pub mod onboard;
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
