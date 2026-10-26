//! What the devices say about themselves, for the settings window's Details:
//! firmware, serial number and wireless id, report interval, backlight,
//! profile, sidetone, and the microphone as blu-ghub last saw it (the
//! headset's equaliser in Windows comes from [`crate::audio_fx`]).
//!
//! What cannot change while a device is plugged in (firmware, serial
//! number, wireless id) is read once per connection worker; the rest each
//! time the device connects or switches on. Read requests only (the
//! sidetone's set is the one write, and only when configured).
//! The serial number and wireless id are never logged; the settings window
//! shows them masked until asked.

use crate::hidpp::{Channel, RECEIVER, feature};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Details {
    /// "MPK 09.04 build 0042"-style lines, one per firmware part.
    pub firmware: Vec<String>,
    pub receiver_firmware: Option<String>,
    pub serial: Option<String>,
    pub wireless_id: Option<String>,
    /// The receiver's pairing record's byte 2, which Solaar shows as the
    /// polling rate in ms.
    pub report_ms: Option<u8>,
    /// The active on-board profile's report rate (its first byte, in ms, as
    /// libratbag and Solaar read profiles).
    pub profile_report_ms: Option<u8>,
    /// Current and highest backlight level.
    pub backlight: Option<(u16, u16)>,
    /// The bank in use: "M2", and whose (the keyboard's profile or blu-ghub's).
    pub profile: Option<String>,
    /// 0-100.
    pub sidetone: Option<u8>,
    /// The equaliser Windows plays the headset through (Logitech's audio
    /// processing), as (Hz, dB) per band.
    pub eq: Option<Vec<(f32, f32)>>,
    /// The microphone's mute in Windows, while blu-ghub handles it; `None`
    /// while the headset mutes by itself and tells nobody.
    pub mic_muted: Option<bool>,
    /// The boom, as last moved (only reported while blu-ghub has the
    /// buttons): `true` up.
    pub boom_up: Option<bool>,
    /// blu-ghub has the headset's buttons, and with them the mute.
    pub mic_handled: bool,
}

fn index(ch: &mut Channel, dev: u8, id: u16) -> Option<u8> {
    ch.feature_index(dev, id).ok().flatten().map(|f| f.0)
}

/// The DEVICE_FW_VERSION entities as text: kind, name, version and build.
fn firmware(ch: &mut Channel, dev: u8) -> Vec<String> {
    let Some(i) = index(ch, dev, feature::DEVICE_FW_VERSION) else { return Vec::new() };
    let Ok(n) = ch.request(dev, i, 0, &[]) else { return Vec::new() };
    let mut out = Vec::new();
    for e in 0..n.first().copied().unwrap_or(0).min(8) {
        if let Ok(p) = ch.request(dev, i, 1, &[e])
            && let Some(line) = entity_text(&p)
        {
            out.push(line);
        }
    }
    out
}

/// One entity: `[type, name × 3, version × 2 (BCD), build × 2, ...]`; an
/// empty name is a part that reports nothing.
fn entity_text(p: &[u8]) -> Option<String> {
    if p.len() < 8 || p[1..4].iter().all(|&c| c == 0) {
        return None;
    }
    let kind = match p[0] & 0x0F {
        0 => "firmware",
        1 => "bootloader",
        2 => "hardware",
        _ => "other",
    };
    let name = String::from_utf8_lossy(&p[1..4]).trim().to_owned();
    Some(format!("{kind} {name} {:02X}.{:02X} build {:02X}{:02X}", p[4], p[5], p[6], p[7]))
}

/// The keyboard's fixed details. `receiver`: it is behind one, whose
/// registers hold the serial number, wireless id and report interval.
pub fn read_keyboard_fixed(ch: &mut Channel, dev: u8, receiver: bool) -> Details {
    let mut d = Details { firmware: firmware(ch, dev), ..Default::default() };
    if receiver {
        // Firmware major.minor, build and bootloader, BCD.
        let reg = |ch: &mut Channel, sel: u8| {
            ch.read_register(RECEIVER, 0xF1, false, [sel, 0, 0]).ok().filter(|p| p.len() >= 3)
        };
        if let (Some(v), Some(b)) = (reg(ch, 1), reg(ch, 2)) {
            let boot = reg(ch, 4).map(|p| format!(", bootloader {:02X}.{:02X}", p[1], p[2]));
            d.receiver_firmware = Some(format!(
                "{:02X}.{:02X} build {:02X}{:02X}{}",
                v[1],
                v[2],
                b[1],
                b[2],
                boot.unwrap_or_default()
            ));
        }
        // Pairing information for the keyboard's slot: report interval
        // and wireless id, then the serial number.
        let slot = dev.saturating_sub(1);
        if let Ok(p) = ch.read_register(RECEIVER, 0xB5, true, [0x20 + slot, 0, 0])
            && p.len() >= 5
            && p[0] == 0x20 + slot
        {
            d.report_ms = Some(p[2]);
            d.wireless_id = Some(format!("{:02X}{:02X}", p[3], p[4]));
        }
        if let Ok(p) = ch.read_register(RECEIVER, 0xB5, true, [0x30 + slot, 0, 0])
            && p.len() >= 5
            && p[0] == 0x30 + slot
        {
            d.serial = Some(p[1..5].iter().map(|b| format!("{b:02X}")).collect());
        }
    }
    d
}

/// The keyboard's backlight and profile, which change.
/// `sector`: the profile to read the report rate from, when blu-ghub knows
/// it (it has the G-keys); otherwise the one the keyboard says is in use.
pub fn read_keyboard_live(ch: &mut Channel, dev: u8, d: &mut Details, sector: Option<u16>) {
    if let Some(i) = index(ch, dev, feature::BRIGHTNESS)
        && let (Ok(info), Ok(now)) = (ch.request(dev, i, 0, &[]), ch.request(dev, i, 1, &[]))
        && info.len() >= 2
        && now.len() >= 2
    {
        d.backlight =
            Some((u16::from_be_bytes([now[0], now[1]]), u16::from_be_bytes([info[0], info[1]])));
    }
    let Some(o) = index(ch, dev, feature::ONBOARD_PROFILES) else { return };
    // The profile in use, as M1-M3 (its place in the directory).
    let current = match ch.request(dev, o, 4, &[]) {
        Ok(cur) if cur.len() >= 2 => Some(u16::from_be_bytes([cur[0], cur[1]])),
        Ok(_) => None,
        Err(e) => {
            tracing::debug!("G915: which profile: {e}");
            None
        }
    };
    if let Some(s) = sector.or(current) {
        match ch.request(dev, o, 5, &[(s >> 8) as u8, s as u8, 0, 0]) {
            Ok(head) => {
                tracing::debug!(
                    "G915: profile sector {s} starts {}",
                    crate::diag::hex(&head[..8.min(head.len())])
                );
                d.profile_report_ms = head.first().copied().filter(|ms| (1..=32).contains(ms));
            }
            Err(e) => tracing::debug!("G915: profile sector {s}: {e}"),
        }
    }
    if let Some(sector) = current
        && let Ok(dir) = ch.request(dev, o, 5, &[0, 0, 0, 0])
    {
        let pos = dir
            .chunks(4)
            .take_while(|e| e.len() == 4 && !(e[0] == 0xFF && e[1] == 0xFF))
            .filter(|e| e[2] != 0)
            .position(|e| u16::from_be_bytes([e[0], e[1]]) == sector);
        if let Some(p) = pos {
            d.profile = Some(format!("M{} (the keyboard's profile)", p + 1));
        }
    }
}

/// The headset's fixed details: its firmware.
pub fn read_headset_fixed(ch: &mut Channel, dev: u8) -> Details {
    Details { firmware: firmware(ch, dev), ..Default::default() }
}

/// The headset's sidetone, which changes.
pub fn read_headset_live(ch: &mut Channel, dev: u8, d: &mut Details) {
    if let Some(i) = index(ch, dev, feature::SIDETONE)
        && let Ok(p) = ch.request(dev, i, 0, &[])
    {
        d.sidetone = p.first().copied();
    }
}

/// Set the sidetone (0-100): SIDETONE fn1. Back to the headset's own when
/// it is switched off and on.
pub fn set_sidetone(ch: &mut Channel, dev: u8, level: u8) -> anyhow::Result<()> {
    let i = index(ch, dev, feature::SIDETONE)
        .ok_or_else(|| anyhow::anyhow!("the headset has no sidetone feature"))?;
    ch.request(dev, i, 1, &[level.min(100)])?;
    Ok(())
}

/// Band names: "40 Hz", "1.2 kHz", "12.5 kHz".
pub fn band_name(hz: f32) -> String {
    if hz >= 1000.0 { format!("{:.1} kHz", hz / 1000.0) } else { format!("{hz:.0} Hz") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firmware_entities_read_as_text() {
        let p = [0x00, b'A', b'B', b'C', 0x12, 0x34, 0x00, 0x56, 0x01];
        assert_eq!(entity_text(&p).unwrap(), "firmware ABC 12.34 build 0056");
        let empty = [0x02, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(entity_text(&empty), None);
    }

    #[test]
    fn bands_are_named_in_hz_or_khz() {
        assert_eq!(band_name(40.0), "40 Hz");
        assert_eq!(band_name(1183.0), "1.2 kHz");
        assert_eq!(band_name(12507.0), "12.5 kHz");
    }
}
