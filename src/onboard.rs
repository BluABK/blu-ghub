//! The G915's on-board profile memory (HID++ feature 0x8100): reading and,
//! carefully, writing it.
//!
//! The keyboard keeps three profiles (sectors 1-3, selected by M1-M3) of 255
//! bytes each, plus a directory in sector 0. Each sector ends in a CRC-CCITT
//! of the rest. The layout is libratbag's "profile format 4"; see
//! docs/protocol-g915.md.
//!
//! Writing is flash, so every write here is guarded: the sector must first
//! read back with a valid checksum, a copy of it must be on disk, and the
//! result is read back and compared. A write can only be made with a
//! [`Backup`], which is only obtainable by saving one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use crate::hid;
use crate::hidpp::{self, Channel, HidppError, RECEIVER, feature};

/// CRC-CCITT (polynomial 0x1021, initial 0xFFFF) over a sector's contents,
/// stored big-endian in its last two bytes.
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

/// Does the sector's stored checksum match its contents?
pub fn crc_ok(sector: &[u8]) -> bool {
    let n = sector.len();
    n >= 2 && u16::from_be_bytes([sector[n - 2], sector[n - 1]]) == crc_ccitt(&sector[..n - 2])
}

/// Recompute and store a sector's checksum after changing it.
pub fn seal(sector: &mut [u8]) {
    let n = sector.len();
    let crc = crc_ccitt(&sector[..n - 2]);
    sector[n - 2..].copy_from_slice(&crc.to_be_bytes());
}

/// Offsets in a profile sector (format 4).
pub const BUTTONS_OFFSET: usize = 32;
pub const SHIFTED_BUTTONS_OFFSET: usize = 96;
/// G1-G5 are buttons 0-4; M1-M3 are 5-7.
pub const G_KEYS: usize = 5;

/// What a button does, as stored in a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    /// A keyboard key with modifiers: `80 02 <modifier bits> <HID usage>`.
    Key { modifiers: u8, usage: u8 },
    /// Nothing: `ff ff ff ff`.
    Disabled,
}

impl Binding {
    pub fn encode(self) -> [u8; 4] {
        match self {
            Binding::Key { modifiers, usage } => [0x80, 0x02, modifiers, usage],
            Binding::Disabled => [0xFF; 4],
        }
    }

    /// Parse "F15", "LCtrl", "off", or "usage:0x6A".
    pub fn parse(s: &str) -> Result<Binding> {
        let lower = s.to_ascii_lowercase();
        let modifier = |bit: u8| Ok(Binding::Key { modifiers: bit, usage: 0 });
        match lower.as_str() {
            "off" | "none" | "disabled" => return Ok(Binding::Disabled),
            "lctrl" => return modifier(0x01),
            "lshift" => return modifier(0x02),
            "lalt" => return modifier(0x04),
            "lwin" => return modifier(0x08),
            "rctrl" => return modifier(0x10),
            "rshift" => return modifier(0x20),
            "ralt" | "altgr" => return modifier(0x40),
            "rwin" => return modifier(0x80),
            _ => {}
        }
        if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            let usage = match n {
                1..=12 => 0x3A + n - 1,
                13..=24 => 0x68 + n - 13,
                _ => bail!("no such function key: {s}"),
            };
            return Ok(Binding::Key { modifiers: 0, usage });
        }
        if let Some(hex) = lower.strip_prefix("usage:0x") {
            let usage = u8::from_str_radix(hex, 16).map_err(|_| anyhow!("bad usage: {s}"))?;
            return Ok(Binding::Key { modifiers: 0, usage });
        }
        bail!("unknown key {s:?} (try F1-F24, LCtrl/LShift/LAlt/LWin, RCtrl/..., off, usage:0xNN)")
    }

    pub fn describe(bytes: &[u8]) -> String {
        match bytes {
            [0xFF, ..] => "off".into(),
            [0x80, 0x02, m, k] => {
                let mods = ["LCtrl", "LShift", "LAlt", "LWin", "RCtrl", "RShift", "AltGr", "RWin"];
                let mut parts: Vec<String> = (0..8)
                    .filter(|b| m & (1 << b) != 0)
                    .map(|b| mods[b as usize].to_string())
                    .collect();
                match *k {
                    0 => {}
                    0x3A..=0x45 => parts.push(format!("F{}", k - 0x3A + 1)),
                    0x68..=0x73 => parts.push(format!("F{}", k - 0x68 + 13)),
                    0x1E..=0x26 => parts.push(format!("{}", k - 0x1E + 1)),
                    0x27 => parts.push("0".into()),
                    k => parts.push(format!("usage 0x{k:02X}")),
                }
                parts.join("+")
            }
            [0x90, 0x0D, _, p] => format!("profile {p}"),
            [0x00, ..] => "macro".into(),
            b => format!("{b:02X?}"),
        }
    }
}

/// Proof that a sector's previous contents are saved: the only way to
/// obtain one is [`Backup::save`].
pub struct Backup {
    pub path: PathBuf,
    sector: u16,
}

impl Backup {
    pub fn save(dir: &Path, sector: u16, data: &[u8]) -> Result<Backup> {
        crate::app_paths::ensure_dir(dir);
        let path = dir.join(format!("sector-{sector:04X}.bin"));
        std::fs::write(&path, data).with_context(|| format!("saving {}", path.display()))?;
        // Make sure it really landed: the backup is the way back.
        let back = std::fs::read(&path).context("re-reading the backup")?;
        if back != data {
            bail!("the backup {} did not read back identically", path.display());
        }
        Ok(Backup { path, sector })
    }
}

/// The G915's on-board memory, over its receiver or its cable.
pub struct Keyboard {
    ch: Channel,
    dev: u8,
    feat: u8,
    pub sector_size: usize,
    pub profiles: u8,
}

impl Keyboard {
    /// Find the G915 (receiver slot holding wireless id 0x407C, or the
    /// cable) and its ONBOARD_PROFILES feature.
    pub fn open() -> Result<Keyboard> {
        let all = hid::logitech_collections()?;
        let (mut ch, dev) = if let Some(cols) = hidpp::hidpp_groups(&all, 0xC33E).into_iter().next()
        {
            (Channel::open(&cols, "G915 cable")?, RECEIVER)
        } else {
            let mut found = None;
            for cols in hidpp::hidpp_groups(&all, 0xC547) {
                let mut ch = Channel::open(&cols, "receiver")?;
                for slot in 0..6u8 {
                    if let Ok(p) = ch.read_register(RECEIVER, 0xB5, true, [0x20 + slot, 0, 0])
                        && p.first() == Some(&(0x20 + slot))
                        && p.len() >= 5
                        && p[3] == 0x40
                        && p[4] == 0x7C
                    {
                        found = Some((ch, slot + 1));
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            found.ok_or_else(|| anyhow!("no G915 found on a receiver or a cable"))?
        };
        let feat = match ch.feature_index(dev, feature::ONBOARD_PROFILES) {
            Ok(Some((i, _, _))) => i,
            Ok(None) => bail!("the keyboard has no on-board profiles"),
            Err(HidppError::Offline(_)) | Err(HidppError::Error10(0x08)) => {
                bail!("the keyboard is asleep: press a key and try again")
            }
            Err(e) => bail!("{e}"),
        };
        let d = ch.request(dev, feat, 0, &[])?;
        let sector_size = u16::from_be_bytes([d[7], d[8]]) as usize;
        if !(16..=4096).contains(&sector_size) {
            bail!("implausible sector size {sector_size}");
        }
        Ok(Keyboard { ch, dev, feat, sector_size, profiles: d[3] })
    }

    /// The keyboard's HID++ channel and device index, for other features.
    pub fn channel(&mut self) -> (&mut Channel, u8) {
        (&mut self.ch, self.dev)
    }

    /// The profile sector currently in use (the active M bank).
    pub fn current_profile(&mut self) -> Result<u16> {
        let p = self.ch.request(self.dev, self.feat, 4, &[])?;
        Ok(u16::from_be_bytes([p[0], p[1]]))
    }

    /// Read a whole sector, 16 bytes at a time; the last read is pulled back
    /// to end exactly at the sector's end.
    pub fn read_sector(&mut self, sector: u16) -> Result<Vec<u8>, HidppError> {
        let size = self.sector_size;
        let mut data = vec![0u8; size];
        let mut offset = 0usize;
        while offset < size {
            let at = offset.min(size - 16);
            let params = [(sector >> 8) as u8, sector as u8, (at >> 8) as u8, at as u8];
            let p = self.ch.request(self.dev, self.feat, 5, &params)?;
            data[at..at + 16].copy_from_slice(&p[..16]);
            offset = at + 16;
        }
        Ok(data)
    }

    /// Write a whole sector (its checksum must already be correct), then
    /// read it back and compare. Requires the backup of what was there.
    pub fn write_sector(&mut self, sector: u16, data: &[u8], backup: &Backup) -> Result<()> {
        if backup.sector != sector {
            bail!("the backup is of sector {:04X}, not {sector:04X}", backup.sector);
        }
        if data.len() != self.sector_size {
            bail!(
                "sector data is {} bytes, the keyboard's sectors are {}",
                data.len(),
                self.sector_size
            );
        }
        if !crc_ok(data) {
            bail!("refusing to write a sector whose checksum does not match its contents");
        }
        let size = self.sector_size as u16;
        let start = [(sector >> 8) as u8, sector as u8, 0, 0, (size >> 8) as u8, size as u8];
        self.ch.request(self.dev, self.feat, 6, &start).context("starting the write")?;
        for chunk in data.chunks(16) {
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            self.ch.request(self.dev, self.feat, 7, &block).context("writing")?;
        }
        // Sent as a short report, as libratbag does.
        self.ch.request_short(self.dev, self.feat, 8, &[]).context("ending the write")?;
        let back = self.read_sector(sector).context("reading the sector back")?;
        if back != data {
            bail!(
                "sector {sector:04X} did not read back as written; restore it from {}",
                backup.path.display()
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_ccitt_matches_the_standard_check_value() {
        // CRC-16/CCITT-FALSE of "123456789" is 0x29B1.
        assert_eq!(crc_ccitt(b"123456789"), 0x29B1);
    }

    #[test]
    fn sealing_makes_a_changed_sector_valid_again() {
        let mut s = vec![0x5Au8; 255];
        seal(&mut s);
        assert!(crc_ok(&s));
        s[40] = 0x6A;
        assert!(!crc_ok(&s));
        seal(&mut s);
        assert!(crc_ok(&s));
    }

    #[test]
    fn bindings_encode_as_the_keyboard_stores_them() {
        assert_eq!(Binding::parse("F1").unwrap().encode(), [0x80, 0x02, 0x00, 0x3A]);
        assert_eq!(Binding::parse("F15").unwrap().encode(), [0x80, 0x02, 0x00, 0x6A]);
        assert_eq!(Binding::parse("f14").unwrap().encode(), [0x80, 0x02, 0x00, 0x69]);
        assert_eq!(Binding::parse("LCtrl").unwrap().encode(), [0x80, 0x02, 0x01, 0x00]);
        assert_eq!(Binding::parse("off").unwrap().encode(), [0xFF; 4]);
        assert!(Binding::parse("F25").is_err());
        assert!(Binding::parse("banana").is_err());
    }

    #[test]
    fn stored_bindings_read_back_as_names() {
        assert_eq!(Binding::describe(&[0x80, 0x02, 0x00, 0x3A]), "F1");
        assert_eq!(Binding::describe(&[0x80, 0x02, 0x00, 0x6A]), "F15");
        assert_eq!(Binding::describe(&[0x80, 0x02, 0x01, 0x00]), "LCtrl");
        assert_eq!(Binding::describe(&[0x80, 0x02, 0x00, 0x23]), "6");
        assert_eq!(Binding::describe(&[0x90, 0x0D, 0x00, 0x02]), "profile 2");
        assert_eq!(Binding::describe(&[0xFF, 0xFF, 0xFF, 0xFF]), "off");
    }
}
