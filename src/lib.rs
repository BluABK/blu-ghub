//! blu-ghub: a small replacement for Logitech G HUB, for one G915 keyboard
//! (on its LIGHTSPEED receiver or its cable) and one G935 headset.
//!
//! - [`app`]: the tray app (BluGHub.exe). Battery levels, read with HID++
//!   requests only when a device connects or wakes, when asked, and as a
//!   30-minute fallback while it is awake. If configured, the G935's G1-G3
//!   as media keys, and with them its microphone mute ([`audio`]). Writes:
//!   the receiver's own connection notifications, taking the headset's
//!   buttons over and back, the headset's lighting, and while the G915
//!   layout editor is open or blu-ghub's layout is to be shown, the
//!   lighting claimed from the keyboard's own effects and per-key colours
//!   in its RAM ([`perkey`]; never stored).
//! - [`diag`]: BluGHub-diag. `caps` reads HID descriptors through handles
//!   opened with no access rights, `sniff` opens the HID++ collections (and
//!   with `--headset-all` the headset's others) read-only, `keys` watches
//!   keyboard input through Raw Input (no hook), `probe`/`dump-onboard` send
//!   read requests only, and three commands write the keyboard's on-board
//!   G-keys.

#[cfg(not(windows))]
compile_error!("blu-ghub talks to the Windows HID stack and only builds on Windows");

pub mod app;
pub mod app_paths;
pub mod audio;
pub mod battery;
pub mod clipboard;
pub mod config;
pub mod diag;
pub mod ghub_import;
pub mod gkeys;
pub mod hid;
pub mod hidpp;
pub mod icon_art;
pub mod inject;
pub mod input;
pub mod kblayout;
pub mod keymap;
pub mod keynames;
pub mod lighting;
pub mod macros;
pub mod onboard;
pub mod perkey;
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
