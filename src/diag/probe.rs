//! `probe`: ask the receiver, the G915 and the G935 what they are and what
//! they support, using only HID++ *read* requests.
//!
//! Every request here is a "get": feature lookups, the feature table, names,
//! firmware versions, battery, the on-board profile mode, lighting zone and
//! effect descriptions. Nothing that changes state is ever sent — no mode
//! switches, no memory writes, no lighting, and none of the functions whose
//! direction the open-source references disagree about. Game Mode (0x4522)
//! is not touched at all: one implementation's "harmless" call clears its key
//! list.
//!
//! `dump-onboard` additionally reads the keyboard's on-board profile memory,
//! sector by sector, into files: the backup that must exist before anything
//! is ever written there.

use anyhow::{Result, bail};

use super::{Tee, hex, now_stamp};
use crate::hid;
use crate::hidpp::{self, Channel, HidppError, RECEIVER, feature as f};

type Out = Tee;

fn show(out: &mut Out, what: &str, r: Result<Vec<u8>, HidppError>) -> Option<Vec<u8>> {
    match r {
        Ok(p) => {
            out.line(&format!("    {what:<44} {}", hex(trim(&p))));
            Some(p)
        }
        Err(e) => {
            out.line(&format!("    {what:<44} ({e})"));
            None
        }
    }
}

/// Replies are zero-padded to 16 bytes; drop the padding for display.
fn trim(p: &[u8]) -> &[u8] {
    let end = p.iter().rposition(|&b| b != 0).map(|i| i + 1).unwrap_or(0);
    &p[..end.max(1).min(p.len())]
}

pub fn run() -> Result<()> {
    let _lock = hidpp::acquire_lock()?;
    let mut out = Tee::create("probe");
    out.line(&format!("BluGHub-diag probe {} at {}", crate::version(), now_stamp()));
    out.line("Read-only HID++ queries. If the keyboard is asleep, press a key and run again.");
    let all = hid::logitech_collections()?;
    let mut any = false;

    for pid in [0xC547u16, 0xC541] {
        for cols in hidpp::hidpp_groups(&all, pid) {
            any = true;
            out.line(&format!("\n=== LIGHTSPEED receiver {pid:04X} ==="));
            let mut ch = Channel::open(&cols, "receiver")?;
            probe_receiver(&mut out, &mut ch);
            for dev in 1..=6u8 {
                match ch.ping(dev) {
                    Ok((maj, min)) => {
                        out.line(&format!(
                            "\n--- device {dev} behind the receiver: HID++ {maj}.{min} ---"
                        ));
                        if maj >= 2 {
                            probe_device(&mut out, &mut ch, dev);
                        }
                    }
                    Err(HidppError::Offline(c)) => {
                        out.line(&format!("  device {dev}: paired but asleep/off (error {c:02X})"))
                    }
                    Err(HidppError::Error10(0x08)) | Err(HidppError::Error10(0x02)) => {}
                    Err(e) => out.line(&format!("  device {dev}: {e}")),
                }
            }
            report_unsolicited(&mut out, &mut ch);
        }
    }

    let wired = hidpp::hidpp_collections(&all, 0xC33E);
    if !wired.is_empty() {
        any = true;
        out.line("\n=== G915 on its USB cable (C33E) ===");
        let mut ch = Channel::open(&wired, "G915 wired")?;
        probe_device(&mut out, &mut ch, RECEIVER);
        report_unsolicited(&mut out, &mut ch);
    }

    let headset = hidpp::hidpp_collections(&all, 0x0A87);
    if !headset.is_empty() {
        any = true;
        out.line("\n=== G935 headset dongle (0A87) ===");
        let mut ch = Channel::open(&headset, "G935")?;
        match ch.ping(RECEIVER) {
            Ok((maj, min)) => {
                out.line(&format!("  HID++ {maj}.{min}"));
                probe_device(&mut out, &mut ch, RECEIVER);
            }
            Err(e) => out.line(&format!("  ping: {e} (headset switched off?)")),
        }
        report_unsolicited(&mut out, &mut ch);
    }

    if !any {
        bail!("no LIGHTSPEED receiver, wired G915 or G935 found");
    }
    out.line(&format!("\nWritten to {}", out.path.display()));
    out.flush();
    Ok(())
}

fn probe_receiver(out: &mut Out, ch: &mut Channel) {
    out.line("  receiver registers (HID++ 1.0 reads):");
    show(out, "0x00 notification flags", ch.read_register(RECEIVER, 0x00, false, [0; 3]));
    show(out, "0x02 connection state", ch.read_register(RECEIVER, 0x02, false, [0; 3]));
    for sel in 1..=4u8 {
        show(
            out,
            &format!("0xF1 firmware info [{sel:02X}]"),
            ch.read_register(RECEIVER, 0xF1, false, [sel, 0, 0]),
        );
    }
    for dev in 1..=2u8 {
        for base in [0x20u8, 0x30, 0x40] {
            show(
                out,
                &format!("0xB5 pairing info [{:02X}]", base + dev - 1),
                ch.read_register(RECEIVER, 0xB5, true, [base + dev - 1, 0, 0]),
            );
        }
    }
}

fn probe_device(out: &mut Out, ch: &mut Channel, dev: u8) {
    let table = match ch.feature_table(dev) {
        Ok(t) => t,
        Err(e) => {
            out.line(&format!("  feature table: {e}"));
            return;
        }
    };
    out.line("  feature table (index: id name [type, version]):");
    for &(i, id, ty, ver) in &table {
        let hidden = if ty & 0x40 != 0 { " hidden" } else { "" };
        let obsolete = if ty & 0x80 != 0 { " obsolete" } else { "" };
        let eng = if ty & 0x20 != 0 { " engineering" } else { "" };
        out.line(&format!(
            "    {i:>2}: {id:04X} {:<34} [type {ty:02X}{hidden}{obsolete}{eng}, v{ver}]",
            f::name(id)
        ));
    }
    let idx = |id: u16| table.iter().find(|t| t.1 == id).map(|t| (t.0, t.3));

    if let Some((i, _)) = idx(f::DEVICE_NAME)
        && let Some(n) = show(out, "DEVICE_NAME length", ch.request(dev, i, 0, &[]))
    {
        let len = n.first().copied().unwrap_or(0) as usize;
        let mut name = Vec::new();
        while name.len() < len {
            match ch.request(dev, i, 1, &[name.len() as u8]) {
                Ok(p) => {
                    let take = (len - name.len()).min(p.len());
                    if take == 0 {
                        break;
                    }
                    name.extend_from_slice(&p[..take]);
                }
                Err(_) => break,
            }
        }
        out.line(&format!("    {:<44} {:?}", "name", String::from_utf8_lossy(&name)));
    }
    if let Some((i, _)) = idx(f::DEVICE_FW_VERSION)
        && let Some(n) = show(out, "FW entity count", ch.request(dev, i, 0, &[]))
    {
        for e in 0..n.first().copied().unwrap_or(0) {
            if let Ok(p) = ch.request(dev, i, 1, &[e]) {
                let kind = match p[0] & 0x0F {
                    0 => "firmware",
                    1 => "bootloader",
                    2 => "hardware",
                    _ => "other",
                };
                out.line(&format!(
                    "    entity {e}: {kind:<10} {}{:02X}.{:02X} build {:02X}{:02X}   [{}]",
                    String::from_utf8_lossy(&p[1..4]),
                    p[4],
                    p[5],
                    p[6],
                    p[7],
                    hex(trim(&p))
                ));
            }
        }
    }

    if let Some((i, _)) = idx(f::BATTERY_VOLTAGE)
        && let Some(p) = show(out, "BATTERY_VOLTAGE get", ch.request(dev, i, 0, &[]))
    {
        let mv = u16::from_be_bytes([p[0], p[1]]);
        out.line(&format!("    {:<44} {mv} mV, flags {:02X}", "", p[2]));
    }
    if let Some((i, _)) = idx(f::UNIFIED_BATTERY) {
        show(out, "UNIFIED_BATTERY capabilities", ch.request(dev, i, 0, &[]));
        show(out, "UNIFIED_BATTERY status", ch.request(dev, i, 1, &[]));
    }
    if let Some((i, _)) = idx(f::BATTERY_STATUS) {
        show(out, "BATTERY_STATUS get", ch.request(dev, i, 0, &[]));
    }
    if let Some((i, ver)) = idx(f::ADC_MEASUREMENT) {
        if let Some(p) = show(out, "ADC_MEASUREMENT get", ch.request(dev, i, 0, &[])) {
            let mv = u16::from_be_bytes([p[0], p[1]]);
            out.line(&format!("    {:<44} {mv} mV, flags {:02X}", "", p[2]));
        }
        if ver >= 2 {
            show(out, "ADC power-off timeout (min)", ch.request(dev, i, 1, &[]));
        }
    }

    if let Some((i, _)) = idx(f::ONBOARD_PROFILES) {
        if let Some(p) = show(out, "ONBOARD_PROFILES description", ch.request(dev, i, 0, &[])) {
            out.line(&format!(
                "    {:<44} memory model {:02X}, profile format {:02X}, macro format {:02X}, \
                 profiles {} (+{} in ROM), buttons {}, sectors {}, sector size {}",
                "",
                p[0],
                p[1],
                p[2],
                p[3],
                p[4],
                p[5],
                p[6],
                u16::from_be_bytes([p[7], p[8]])
            ));
        }
        if let Some(p) = show(out, "ONBOARD_PROFILES mode", ch.request(dev, i, 2, &[])) {
            let m = match p[0] {
                1 => "on-board",
                2 => "host",
                _ => "?",
            };
            out.line(&format!("    {:<44} {m}", ""));
        }
        show(out, "ONBOARD_PROFILES current profile", ch.request(dev, i, 4, &[]));
    }

    if let Some((i, _)) = idx(f::PER_KEY_LIGHTING_V2) {
        let mut zones = Vec::new();
        for page in 0..3u8 {
            if let Some(p) = show(
                out,
                &format!("PER_KEY_LIGHTING_V2 info page {page}"),
                ch.request(dev, i, 0, &[0, page]),
            ) {
                for (byte, &b) in p.iter().enumerate().skip(2).take(14) {
                    for bit in 0..8 {
                        if b & (1 << bit) != 0 {
                            zones.push(page as u16 * 112 + (byte as u16 - 2) * 8 + bit);
                        }
                    }
                }
            }
        }
        out.line(&format!("    {} zones: {:?}", zones.len(), zones));
    }
    if let Some((i, _)) = idx(f::RGB_EFFECTS) {
        if let Some(p) = show(
            out,
            "RGB_EFFECTS device info [FF FF 00]",
            ch.request(dev, i, 0, &[0xFF, 0xFF, 0x00]),
        ) {
            let clusters = p.get(2).copied().unwrap_or(0).min(4);
            for c in 0..clusters {
                if let Some(z) = show(
                    out,
                    &format!("RGB_EFFECTS cluster {c} [{c:02X} FF 00]"),
                    ch.request(dev, i, 0, &[c, 0xFF, 0x00]),
                ) {
                    let effects = z.get(4).copied().unwrap_or(0).min(12);
                    for e in 0..effects {
                        show(
                            out,
                            &format!("RGB_EFFECTS cluster {c} effect {e}"),
                            ch.request(dev, i, 0, &[c, e, 0x00]),
                        );
                    }
                }
            }
        }
        show(out, "RGB_EFFECTS software control (get)", ch.request(dev, i, 5, &[0x00]));
        show(out, "RGB_EFFECTS power config (get)", ch.request(dev, i, 7, &[0x00]));
    }
    if let Some((i, _)) = idx(f::BRIGHTNESS) {
        show(out, "BRIGHTNESS info", ch.request(dev, i, 0, &[]));
        show(out, "BRIGHTNESS get", ch.request(dev, i, 1, &[]));
    }
    if let Some((i, _)) = idx(f::GKEY) {
        show(out, "GKEY count", ch.request(dev, i, 0, &[]));
    }
    if let Some((i, _)) = idx(f::MKEYS) {
        show(out, "MKEYS count", ch.request(dev, i, 0, &[]));
    }

    if let Some((i, _)) = idx(f::COLOR_LED_EFFECTS)
        && let Some(p) = show(out, "COLOR_LED_EFFECTS info", ch.request(dev, i, 0, &[]))
    {
        let zones = p[0].min(4);
        for z in 0..zones {
            if let Some(zi) =
                show(out, &format!("COLOR_LED_EFFECTS zone {z} info"), ch.request(dev, i, 1, &[z]))
            {
                let loc = match u16::from_be_bytes([zi[1], zi[2]]) {
                    1 => "primary",
                    2 => "logo",
                    _ => "?",
                };
                out.line(&format!(
                    "    {:<44} location {loc}, {} effects, persistency caps {:02X}",
                    "", zi[3], zi[4]
                ));
                for e in 0..zi[3].min(12) {
                    if let Some(ei) = show(
                        out,
                        &format!("  zone {z} effect {e} info"),
                        ch.request(dev, i, 2, &[z, e]),
                    ) {
                        let id = u16::from_be_bytes([ei[2], ei[3]]);
                        let name = match id {
                            0x0000 => "off",
                            0x0001 => "static",
                            0x0003 => "cycle",
                            0x000A => "breathing",
                            _ => "?",
                        };
                        out.line(&format!("    {:<44} id {id:04X} ({name})", ""));
                    }
                }
                show(
                    out,
                    &format!("COLOR_LED_EFFECTS zone {z} current"),
                    ch.request(dev, i, 0x0E, &[z]),
                );
            }
        }
    }
    if let Some((i, _)) = idx(f::SIDETONE) {
        show(out, "SIDETONE level", ch.request(dev, i, 0, &[]));
    }
    if let Some((i, _)) = idx(f::EQUALIZER) {
        show(out, "EQUALIZER info", ch.request(dev, i, 0, &[]));
        show(out, "EQUALIZER bands 0-6", ch.request(dev, i, 1, &[0]));
        show(out, "EQUALIZER bands 7-", ch.request(dev, i, 1, &[7]));
        show(out, "EQUALIZER current gains", ch.request(dev, i, 2, &[0]));
    }
}

/// Anything the devices sent on their own while we were asking.
fn report_unsolicited(out: &mut Out, ch: &mut Channel) {
    let incoming = ch.take_incoming();
    if incoming.is_empty() {
        return;
    }
    out.line(&format!("  {} report(s) arrived that were not replies to us:", incoming.len()));
    for r in incoming.iter().take(40) {
        out.line(&format!(
            "    {:<44} {}",
            super::sniff::decode(&r.data).to_string(),
            hex(&r.data)
        ));
    }
}

/// CRC-CCITT (polynomial 0x1021, initial 0xFFFF) over a sector's contents,
/// as the on-board profile format stores in each sector's last two bytes.
pub fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= (b as u16) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

/// Read the G915's on-board profile memory into files: one `.bin` per
/// sector, plus an index with each sector's stored and computed checksum.
pub fn dump_onboard() -> Result<()> {
    let _lock = hidpp::acquire_lock()?;
    let mut out = Tee::create("onboard");
    out.line(&format!("BluGHub-diag dump-onboard {} at {}", crate::version(), now_stamp()));
    let all = hid::logitech_collections()?;

    let (mut ch, dev) = if let cols @ [_, ..] = hidpp::hidpp_collections(&all, 0xC547).as_slice() {
        (Channel::open(cols, "receiver")?, 1u8)
    } else if let cols @ [_, ..] = hidpp::hidpp_collections(&all, 0xC33E).as_slice() {
        (Channel::open(cols, "G915 wired")?, RECEIVER)
    } else {
        bail!("no G915 found (receiver C547 or cable C33E)");
    };
    let Some((i, _, _)) = ch.feature_index(dev, f::ONBOARD_PROFILES)? else {
        bail!("the keyboard has no ONBOARD_PROFILES feature");
    };
    let d = ch.request(dev, i, 0, &[])?;
    let (profiles, rom_profiles, sectors) = (d[3], d[4], d[6]);
    let size = u16::from_be_bytes([d[7], d[8]]) as usize;
    out.line(&format!(
        "description: {}  (profiles {profiles}, ROM profiles {rom_profiles}, sectors {sectors}, \
         sector size {size})",
        hex(trim(&d))
    ));
    if !(16..=4096).contains(&size) {
        bail!("implausible sector size {size}");
    }

    let dir = out.path.with_extension("");
    crate::app_paths::ensure_dir(&dir);
    let mut list: Vec<u16> = (0..sectors as u16).collect();
    list.extend((0..rom_profiles as u16).map(|n| 0x0100 + n));
    for sector in list {
        let mut data = vec![0u8; size];
        let mut offset = 0usize;
        let mut failed = None;
        while offset < size {
            // Reads are 16 bytes; the last one is pulled back so it ends
            // exactly at the sector's end (the device refuses reads past it).
            let at = offset.min(size - 16);
            let params = [(sector >> 8) as u8, sector as u8, (at >> 8) as u8, at as u8];
            match ch.request(dev, i, 5, &params) {
                Ok(p) => data[at..at + 16].copy_from_slice(&p[..16]),
                Err(e) => {
                    failed = Some(e);
                    break;
                }
            }
            offset = at + 16;
        }
        if let Some(e) = failed {
            out.line(&format!("sector {sector:04X}: {e}"));
            continue;
        }
        let stored = u16::from_be_bytes([data[size - 2], data[size - 1]]);
        let computed = crc_ccitt(&data[..size - 2]);
        let empty = data.iter().all(|&b| b == 0xFF);
        let path = dir.join(format!("sector-{sector:04X}.bin"));
        std::fs::write(&path, &data)?;
        out.line(&format!(
            "sector {sector:04X}: {size} bytes, crc stored {stored:04X} computed {computed:04X} {}{}",
            if stored == computed { "OK" } else { "MISMATCH" },
            if empty { " (erased)" } else { "" }
        ));
    }
    out.line(&format!("Sectors written to {}", dir.display()));
    out.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_ccitt_matches_the_standard_check_value() {
        // CRC-16/CCITT-FALSE of "123456789" is 0x29B1.
        assert_eq!(crc_ccitt(b"123456789"), 0x29B1);
    }
}
