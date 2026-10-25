//! On-board memory commands: back up, test-write, change G-keys, restore.
//!
//! Every write goes through [`onboard::Keyboard::write_sector`], which needs
//! a saved backup and reads the result back. Each run saves the sector's
//! previous contents to `%APPDATA%\BluGHub\diag\<command>-<time>\` first.

use std::path::Path;

use anyhow::{Context, Result, bail};

use super::{Tee, hex, now_stamp};
use crate::hidpp;
use crate::onboard::{self, BUTTONS_OFFSET, Backup, Binding, G_KEYS, Keyboard};

/// One line per G-key and M-key of a profile sector.
fn describe_buttons(out: &mut Tee, data: &[u8]) {
    let names = ["G1", "G2", "G3", "G4", "G5", "M1", "M2", "M3"];
    let line: Vec<String> = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let o = BUTTONS_OFFSET + 4 * i;
            format!("{n}={}", Binding::describe(&data[o..o + 4]))
        })
        .collect();
    out.line(&format!("    {}", line.join("  ")));
}

/// Read `sector`, check its checksum, and save it. Everything that writes
/// starts here.
fn read_and_back_up(kb: &mut Keyboard, sector: u16, out: &mut Tee) -> Result<(Vec<u8>, Backup)> {
    let data = kb.read_sector(sector).with_context(|| format!("reading sector {sector:04X}"))?;
    if !onboard::crc_ok(&data) {
        bail!("sector {sector:04X} does not read back with a valid checksum; not touching it");
    }
    let dir = out.path.with_extension("");
    let backup = Backup::save(&dir, sector, &data)?;
    out.line(&format!("  backed up sector {sector:04X} to {}", backup.path.display()));
    Ok((data, backup))
}

fn open(prefix: &str) -> Result<(hidpp::HidppLock, Keyboard, Tee)> {
    let lock = hidpp::acquire_lock()?;
    let mut out = Tee::create(prefix);
    out.line(&format!("BluGHub-diag {prefix} {} at {}", crate::version(), now_stamp()));
    let mut kb = Keyboard::open()?;
    let current = kb.current_profile().unwrap_or(0);
    out.line(&format!(
        "  keyboard: {} profiles, sectors of {} bytes, active profile in sector {current:04X}",
        kb.profiles, kb.sector_size
    ));
    Ok((lock, kb, out))
}

/// Write `sector` back exactly as it is, and check it reads back the same.
/// Proves the write path before anything is changed.
pub fn write_test(sector: u16) -> Result<()> {
    let (_lock, mut kb, mut out) = open("write-test")?;
    let (data, backup) = read_and_back_up(&mut kb, sector, &mut out)?;
    describe_buttons(&mut out, &data);
    out.line(&format!("  writing sector {sector:04X} back unchanged..."));
    kb.write_sector(sector, &data, &backup)?;
    out.line("  OK: written and read back identically.");
    out.flush();
    Ok(())
}

/// Change G-keys in profile `sector`: `assignments` like `["G3=F15", "G5=LCtrl"]`.
pub fn set_gkeys(sector: u16, assignments: &[&str]) -> Result<()> {
    let mut changes = Vec::new();
    for a in assignments {
        let Some((key, value)) = a.split_once('=') else { bail!("expected Gn=KEY, got {a:?}") };
        let n: usize = key
            .trim()
            .to_ascii_uppercase()
            .strip_prefix('G')
            .and_then(|n| n.parse().ok())
            .filter(|n| (1..=G_KEYS).contains(n))
            .ok_or_else(|| anyhow::anyhow!("expected G1-G{G_KEYS}, got {key:?}"))?;
        changes.push((n - 1, Binding::parse(value.trim())?));
    }
    if changes.is_empty() {
        bail!("nothing to change");
    }
    if !(1..=3).contains(&sector) {
        bail!("profiles are in sectors 1-3");
    }
    let (_lock, mut kb, mut out) = open("set-gkeys")?;
    let (mut data, backup) = read_and_back_up(&mut kb, sector, &mut out)?;
    out.line("  before:");
    describe_buttons(&mut out, &data);
    for (i, b) in &changes {
        let o = BUTTONS_OFFSET + 4 * i;
        data[o..o + 4].copy_from_slice(&b.encode());
    }
    onboard::seal(&mut data);
    out.line("  after:");
    describe_buttons(&mut out, &data);
    kb.write_sector(sector, &data, &backup)?;
    out.line(&format!("  OK: sector {sector:04X} written and read back identically."));
    out.line(&format!(
        "  To undo: BluGHub-diag restore-sector {sector} \"{}\"",
        backup.path.display()
    ));
    out.flush();
    Ok(())
}

/// Write a saved sector back (undo).
pub fn restore(sector: u16, file: &Path) -> Result<()> {
    let saved = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    if !onboard::crc_ok(&saved) {
        bail!("{} does not have a valid checksum; not writing it", file.display());
    }
    let (_lock, mut kb, mut out) = open("restore-sector")?;
    if saved.len() != kb.sector_size {
        bail!(
            "{} is {} bytes; the keyboard's sectors are {}",
            file.display(),
            saved.len(),
            kb.sector_size
        );
    }
    let (_current, backup) = read_and_back_up(&mut kb, sector, &mut out)?;
    out.line("  restoring:");
    describe_buttons(&mut out, &saved);
    kb.write_sector(sector, &saved, &backup)?;
    out.line(&format!("  OK: sector {sector:04X} restored and read back identically."));
    out.flush();
    Ok(())
}

/// Read the whole on-board memory into one file per sector, with checksums.
pub fn dump() -> Result<()> {
    let (_lock, mut kb, mut out) = open("onboard")?;
    let dir = out.path.with_extension("");
    crate::app_paths::ensure_dir(&dir);
    let mut list: Vec<u16> = (0..16).collect();
    list.extend((0..kb.profiles as u16).map(|n| 0x0100 + n));
    for sector in list {
        match kb.read_sector(sector) {
            Ok(data) => {
                let erased = data.iter().all(|&b| b == 0xFF);
                std::fs::write(dir.join(format!("sector-{sector:04X}.bin")), &data)?;
                let status = if erased {
                    "erased"
                } else if onboard::crc_ok(&data) {
                    "crc OK"
                } else {
                    "crc MISMATCH"
                };
                out.line(&format!("  sector {sector:04X}: {status}"));
                if (1..=3).contains(&sector) && !erased {
                    describe_buttons(&mut out, &data);
                }
                out.file_only(&format!("    {}", hex(&data)));
            }
            Err(e) => out.line(&format!("  sector {sector:04X}: {e}")),
        }
    }
    out.line(&format!("  sectors written to {}", dir.display()));
    out.flush();
    Ok(())
}
