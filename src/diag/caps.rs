//! `caps`: list every Logitech HID collection with its usage and report
//! lengths, read from the descriptor through handles opened with no access
//! rights. Nothing is sent to any device.
//!
//! This settles the transport questions before any code depends on them:
//! which collection is the HID++ short channel (7-byte reports) and which the
//! long one (20 bytes), and that the G935's HID++ collection takes only long
//! reports.

use anyhow::Result;

use super::Tee;
use crate::hid;

/// What a collection is, as far as blu-ghub is concerned.
pub fn role(i: &hid::HidInfo) -> &'static str {
    match (i.pid, i.usage_page, i.usage) {
        (0xC547 | 0xC33E | 0xC541, 0xFF00, 0x0001) => "HID++ short (0x10)",
        (0xC547 | 0xC33E | 0xC541, 0xFF00, 0x0002) => "HID++ long (0x11)",
        (0x0A87, 0xFF43, 0x0202) => "HID++ long (0x11), headset",
        (0x0A87, 0xFF00, _) => "vendor channel, not HID++ (never write)",
        (_, 0x0001, 0x0006) => "keyboard",
        (_, 0x0001, 0x0002) => "mouse",
        (_, 0x0001, 0x0080) => "system control",
        (_, 0x000C, _) => "consumer control",
        (_, 0x000B, _) => "telephony",
        (_, 0x0059, _) => "LampArray (Dynamic Lighting)",
        _ => "",
    }
}

pub fn run() -> Result<()> {
    let mut out = Tee::create("caps");
    out.line(&format!("BluGHub-diag caps {} at {}", crate::version(), super::now_stamp()));
    let all = hid::logitech_collections()?;
    if all.is_empty() {
        out.line("No Logitech HID collections found.");
    }
    out.line(&format!(
        "{:<26} {:>6} {:>6} {:>4} {:>4} {:>4}  {:<20} {}",
        "collection", "page", "usage", "in", "out", "feat", "product", "role"
    ));
    for i in &all {
        out.line(&format!(
            "{:<26} {:>6} {:>6} {:>4} {:>4} {:>4}  {:<20} {}",
            i.short(),
            format!("{:04X}", i.usage_page),
            format!("{:04X}", i.usage),
            i.input_len,
            i.output_len,
            i.feature_len,
            truncate(&i.product, 20),
            role(i),
        ));
        out.file_only(&format!("    {}", i.path));
    }
    out.line(&format!("\nWritten to {}", out.path.display()));
    out.flush();
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}
