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

/// Names for the usages the devices' plain (non-HID++) collections use.
pub fn usage_name(page: u16, usage: u16) -> &'static str {
    match (page, usage) {
        (0x08, 0x09) => "LED Mute",
        (0x08, 0x17) => "LED Off-Hook",
        (0x08, 0x18) => "LED Ring",
        (0x09, _) => "Button",
        (0x0B, 0x01) => "Phone",
        (0x0B, 0x05) => "Headset",
        (0x0B, 0x07) => "Programmable Button",
        (0x0B, 0x20) => "Hook Switch",
        (0x0B, 0x21) => "Flash",
        (0x0B, 0x24) => "Redial",
        (0x0B, 0x2A) => "Line",
        (0x0B, 0x2B) => "Speaker Phone",
        (0x0B, 0x2F) => "Phone Mute",
        (0x0C, 0xB5) => "Scan Next Track",
        (0x0C, 0xB6) => "Scan Previous Track",
        (0x0C, 0xB7) => "Stop",
        (0x0C, 0xCD) => "Play/Pause",
        (0x0C, 0xE2) => "Mute",
        (0x0C, 0xE9) => "Volume Increment",
        (0x0C, 0xEA) => "Volume Decrement",
        (0xFF00..=0xFFFF, _) => "vendor",
        _ => "",
    }
}

/// `0B:002F Phone Mute`, or just the numbers for a usage without a name.
pub fn usage_text(page: u16, usage: u16) -> String {
    match usage_name(page, usage) {
        "" => format!("{page:02X}:{usage:04X}"),
        name => format!("{page:02X}:{usage:04X} {name}"),
    }
}

/// One descriptor item, e.g. `in 08: 0B:002F Phone Mute (button, relative)`.
pub fn item_text(i: &hid::InputItem) -> String {
    let usage = if i.usage == i.usage_max {
        usage_text(i.page, i.usage)
    } else {
        format!("{}..{:04X}", usage_text(i.page, i.usage), i.usage_max)
    };
    let kind = if i.button { "button" } else { "value" };
    let relative = if i.relative { ", relative" } else { "" };
    format!("in {:02X}: {usage} ({kind}{relative})", i.report_id)
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
        if let Ok(d) = hid::Descriptor::read(&i.path) {
            for item in d.inputs() {
                out.line(&format!("      {}", item_text(&item)));
            }
        }
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
