//! `power-config`: the G915's lighting timers, 0x8071 fn7.
//!
//! fn7 `[00]` reads them: `[getOrSet, flags (2), idle (2), sleep (2)]`, in
//! seconds, big-endian. Idle is when the keyboard dims its lighting, sleep
//! when it switches it off (counted from the last key, not from the dim).
//! `[01, 00 00, idle, sleep]` sets them, as Solaar does.
//!
//! Without arguments this only reads. `set IDLE SLEEP` reads, asks for the
//! word SET, sets, and reads back. Whether the keyboard keeps the values
//! over a power cycle is what the test is for: run it again without
//! arguments after switching the keyboard off and on.

use std::io::BufRead;

use anyhow::{Result, bail};

use super::{Tee, hex, now_stamp};
use crate::hidpp::{self, Channel, feature};
use crate::onboard::Keyboard;

/// (idle, sleep) in seconds, from a fn7 reply.
fn parse(r: &[u8]) -> Option<(u16, u16)> {
    (r.len() >= 7).then(|| (u16::from_be_bytes([r[3], r[4]]), u16::from_be_bytes([r[5], r[6]])))
}

fn request(idle: u16, sleep: u16) -> [u8; 7] {
    let [ih, il] = idle.to_be_bytes();
    let [sh, sl] = sleep.to_be_bytes();
    [0x01, 0x00, 0x00, ih, il, sh, sl]
}

fn read(ch: &mut Channel, dev: u8, fx: u8, out: &mut Tee) -> Result<Option<(u16, u16)>> {
    let r = ch.request(dev, fx, 7, &[0x00])?;
    let v = parse(&r);
    match v {
        Some((idle, sleep)) => out.line(&format!(
            "  reply {}: dims after {idle} s, lighting off after {sleep} s",
            hex(&r[..7])
        )),
        None => out.line(&format!("  reply {} (too short to read)", hex(&r))),
    }
    out.flush();
    Ok(v)
}

fn seconds(s: &str) -> Result<u16> {
    s.parse().map_err(|_| anyhow::anyhow!("not a number of seconds (0-65535): {s}"))
}

pub fn run(args: &[&str]) -> Result<()> {
    let set = match args {
        [] => None,
        ["set", idle, sleep] => Some((seconds(idle)?, seconds(sleep)?)),
        _ => bail!("usage: BluGHub-diag power-config [set IDLE SLEEP]  (seconds)"),
    };
    let _lock = hidpp::acquire_lock()?;
    let mut out = Tee::create("power-config");
    out.line(&format!("BluGHub-diag power-config {} at {}", crate::version(), now_stamp()));
    let mut kb = Keyboard::open()?;
    let (ch, dev) = kb.channel();
    let Some((fx, _, _)) = ch.feature_index(dev, feature::RGB_EFFECTS)? else {
        bail!("the keyboard has no RGB_EFFECTS feature")
    };
    out.line("  now:");
    let before = read(ch, dev, fx, &mut out)?;
    let Some((idle, sleep)) = set else {
        out.line(&format!("Written to {}", out.path.display()));
        out.flush();
        return Ok(());
    };
    if let Some((i, s)) = before {
        out.line(&format!("  to put these back: BluGHub-diag power-config set {i} {s}"));
    }
    out.line("");
    out.line(&format!(
        "  This sets the keyboard to dim after {idle} s and switch its lighting off after {sleep} \
         s (0x8071 fn7 {}).",
        hex(&request(idle, sleep))
    ));
    out.line("  The keyboard may keep it in its own memory; nothing else is changed.");
    out.console("  Type SET and press Enter to set it (anything else stops): ");
    out.flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    let answer = answer.trim();
    out.file_only(&format!("  answer: {answer:?}"));
    if answer != "SET" {
        out.line("  not set; nothing sent");
        out.flush();
        return Ok(());
    }
    let r = ch.request(dev, fx, 7, &request(idle, sleep))?;
    out.line(&format!("  set: reply {}", hex(&r[..r.len().min(7)])));
    out.line("  reads back:");
    match read(ch, dev, fx, &mut out)? {
        Some(v) if v == (idle, sleep) => out.line("  taken"),
        Some(_) => out.line("  NOT the values sent"),
        None => {}
    }
    out.line("");
    out.line("  Now leave the keyboard alone and time the dim and the lights going off.");
    out.line("  Then switch it off and on and run `BluGHub-diag power-config` again: does it");
    out.line("  still read the new values?");
    out.line(&format!("Written to {}", out.path.display()));
    out.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timers_read_and_write_in_seconds() {
        assert_eq!(parse(&[0, 0, 0, 0x00, 0x2D, 0x02, 0x58]), Some((45, 600)));
        assert_eq!(parse(&[0, 0, 0]), None);
        assert_eq!(request(45, 600), [0x01, 0x00, 0x00, 0x00, 0x2D, 0x02, 0x58]);
    }
}
