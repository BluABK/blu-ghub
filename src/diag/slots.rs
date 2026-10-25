//! `slots`: what the G915 holds in its stored-lighting slots, by HID++ *read*
//! requests only.
//!
//! The per-key layout the keyboard shows by itself is the 0x8071 "custom
//! onboard stored" effect (0x000C), played from a slot in the keyboard's
//! memory; a profile's lighting entry only names the slot. 0x8071 getInfo
//! (fn0) with typeOfInfo 1 describes a slot: its state, its defaults, a
//! 16-byte UUID and a 32-character name, which is how G HUB tells which of
//! its effects is stored there. The colours themselves cannot be read back.
//! Nothing here changes any state.

use anyhow::{Result, bail};

use super::{Tee, hex, now_stamp};
use crate::hidpp::{self, Channel, HidppError, feature};
use crate::onboard::Keyboard;

/// Effect id of "custom onboard stored".
const CUSTOM: u16 = 0x000C;
/// Slots to ask about; the G915 is documented with two ("KEY 8", "KEY 9").
const SLOTS: u8 = 4;
/// The profile sector's lighting entries: logo, keys, then the power-save pair.
const LIGHTING: usize = 208;
const ENTRY: usize = 11;

pub fn run() -> Result<()> {
    let _lock = hidpp::acquire_lock()?;
    let mut out = Tee::create("slots");
    out.line(&format!("BluGHub-diag slots {} at {}", crate::version(), now_stamp()));
    let mut kb = Keyboard::open()?;
    let (ch, dev) = kb.channel();
    let Some((fx, _, _)) = ch.feature_index(dev, feature::RGB_EFFECTS)? else {
        bail!("the keyboard has no RGB_EFFECTS feature")
    };
    out.line(&format!("  RGB_EFFECTS at {fx:#04x}"));

    let Some((cluster, effect)) = find_custom(ch, dev, fx, &mut out)? else {
        out.line("  no cluster offers the custom stored effect (0x000C)");
        out.flush();
        return Ok(());
    };
    out.line(&format!("  custom stored effect: cluster {cluster}, effect index {effect}"));

    // The spec addresses an effect by its cluster; G HUB asks a G815 with
    // "all clusters" (FF) and effect 0 instead. Both are reads.
    let mut addressings = vec![(cluster, effect), (0xFF, 0x00), (0xFF, effect)];
    addressings.dedup();
    for (cluster, effect) in addressings {
        out.line("");
        let general = ch.request(dev, fx, 0, &[cluster, effect, 0x00]);
        out.line(&format!(
            "  asking as [{cluster:02X} {effect:02X}] (effect info: {})",
            match &general {
                Ok(r) => hex(&r[..8.min(r.len())]),
                Err(e) => e.to_string(),
            }
        ));
        slots(ch, dev, fx, cluster, effect, &mut out);
    }

    out.line("");
    match kb.current_profile() {
        Ok(sector) => match kb.read_sector(sector) {
            Ok(data) => {
                out.line(&format!("  active profile: sector {sector}"));
                for (i, what) in
                    ["logo", "keys", "logo (power-save)", "keys (power-save)"].iter().enumerate()
                {
                    let at = LIGHTING + i * ENTRY;
                    let Some(e) = data.get(at..at + ENTRY) else { continue };
                    out.line(&format!("    {what:<18} {}", hex(e)));
                    // libratbag's layout of the custom effect's entry.
                    if let [0x0C, slot, ih, il, lh, ll, ph, pl, intensity, ..] = e[..] {
                        out.line(&format!(
                            "    {:<18} custom: slot {slot}, start frame {}, {} frames, {} ms a \
                             frame, intensity {intensity}",
                            "",
                            u16::from_be_bytes([ih, il]),
                            u16::from_be_bytes([lh, ll]),
                            u16::from_be_bytes([ph, pl]),
                        ));
                    }
                }
            }
            Err(e) => out.line(&format!("  active profile sector {sector}: {e}")),
        },
        Err(e) => out.line(&format!("  active profile: {e}")),
    }
    out.line(&format!("Written to {}", out.path.display()));
    out.flush();
    Ok(())
}

/// Every slot's state, defaults, UUID and name, as one addressing sees them.
fn slots(ch: &mut Channel, dev: u8, fx: u8, cluster: u8, effect: u8, out: &mut Tee) {
    for slot in 0..SLOTS {
        let ask = |ch: &mut Channel, kind: u8| {
            ch.request(dev, fx, 0, &[cluster, effect, 0x01, slot, kind]).map(|r| {
                // Echo: cluster, effect, typeOfInfo, slot, kind; then the data.
                r.get(5..).map(<[u8]>::to_vec).unwrap_or_default()
            })
        };
        let state = match ask(ch, 0) {
            Ok(d) => d,
            Err(HidppError::Error20(c)) => {
                out.line(&format!("  slot {slot}: none (error {c:02X})"));
                continue;
            }
            Err(e) => {
                out.line(&format!("  slot {slot}: {e}"));
                continue;
            }
        };
        out.line(&format!("  slot {slot}"));
        out.line(&format!("    state     {}", hex(&state)));
        if let [valid, hi, lo, ..] = state[..] {
            out.line(&format!(
                "              data {}, {} frames",
                if valid != 0 { "valid" } else { "not valid" },
                u16::from_be_bytes([hi, lo])
            ));
        }
        match ask(ch, 1) {
            Ok(d) => {
                out.line(&format!("    defaults  {}", hex(&d)));
                if let [ih, il, lh, ll, ph, pl, intensity, ..] = d[..] {
                    out.line(&format!(
                        "              start frame {}, {} frames, {} ms a frame, intensity {intensity}",
                        u16::from_be_bytes([ih, il]),
                        u16::from_be_bytes([lh, ll]),
                        u16::from_be_bytes([ph, pl]),
                    ));
                }
            }
            Err(e) => out.line(&format!("    defaults  ({e})")),
        }
        // UUID bytes 0-10, then 11-15.
        let mut uuid = Vec::new();
        for (kind, take) in [(2, 11), (3, 5)] {
            match ask(ch, kind) {
                Ok(d) => uuid.extend(d.iter().take(take)),
                Err(e) => out.line(&format!("    uuid part {kind}: ({e})")),
            }
        }
        if uuid.len() == 16 {
            out.line(&format!("    uuid      {}  ({})", uuid_text(&uuid), hex(&uuid)));
        }
        // Name characters 0-10, 11-21, 22-31.
        let mut name = Vec::new();
        for (kind, take) in [(4, 11), (5, 11), (6, 10)] {
            match ask(ch, kind) {
                Ok(d) => name.extend(d.iter().take(take)),
                Err(e) => out.line(&format!("    name part {kind}: ({e})")),
            }
        }
        out.line(&format!("    name      {:?}  ({})", name_text(&name), hex(&name)));
        out.flush();
    }
}

/// The (cluster, effect index) that plays the custom stored effect.
fn find_custom(ch: &mut Channel, dev: u8, fx: u8, out: &mut Tee) -> Result<Option<(u8, u8)>> {
    let info = ch.request(dev, fx, 0, &[0xFF, 0xFF, 0x00])?;
    out.line(&format!("  device info {}", hex(&info[..8.min(info.len())])));
    let clusters = info.get(2).copied().unwrap_or(0).min(8);
    for c in 0..clusters {
        let z = match ch.request(dev, fx, 0, &[c, 0xFF, 0x00]) {
            Ok(z) => z,
            Err(_) => continue,
        };
        let effects = z.get(4).copied().unwrap_or(0).min(16);
        for e in 0..effects {
            if let Ok(r) = ch.request(dev, fx, 0, &[c, e, 0x00])
                && let [_, _, hi, lo, ..] = r[..]
                && u16::from_be_bytes([hi, lo]) == CUSTOM
            {
                return Ok(Some((c, e)));
            }
        }
    }
    Ok(None)
}

/// 8-4-4-4-12, as G HUB writes its effect ids.
fn uuid_text(b: &[u8]) -> String {
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Up to the first NUL or FF.
fn name_text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0 || c == 0xFF).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uuid_is_written_in_groups() {
        let b: Vec<u8> = (0..16).collect();
        assert_eq!(uuid_text(&b), "00010203-0405-0607-0809-0a0b0c0d0e0f");
    }

    #[test]
    fn name_stops_at_padding() {
        assert_eq!(name_text(b"Lights\0\0junk"), "Lights");
        assert_eq!(name_text(&[b'A', 0xFF, b'B']), "A");
    }
}
