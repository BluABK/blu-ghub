//! `host-test`: what the G915 does in host mode, switched back afterwards.
//!
//! 0x8100 fn1 sets the keyboard's mode: 1 on-board (it runs its stored
//! profiles: lighting, G-keys, M keys), 2 host (software drives it, as G
//! HUB does with its on-board memory switched off). fn2 reads it. libratbag
//! uses the same pair, and itself puts a keyboard it finds in host mode back
//! into on-board mode.
//!
//! The test reads the mode, asks for the word HOST before switching, then
//! asks for the M1 light (0x8020 fn1) and paints Escape red (0x8081, RAM
//! only), and switches back to on-board mode on Enter. `--leave` skips the
//! switch back, to see whether a power cycle ends host mode by itself;
//! `--restore` only reads the mode, puts the keyboard back into on-board
//! mode and hands the G-keys back. `--divert` also takes the G-keys (0x8010
//! fn2, as the tray app does) before asking for the M1 light, waits 90 s
//! with the keyboard untouched (the on-board effect overdraws a painted key
//! after about a minute idle), asks for the M3 light, and hands the G-keys
//! back before switching to on-board mode. Nothing else is written; no
//! profile, sector or stored lighting.

use std::io::BufRead;
use std::time::Duration;

use anyhow::{Result, bail};

use super::{Tee, hex, now_stamp};
use crate::hidpp::{self, Channel, feature};
use crate::lighting::Rgb;
use crate::onboard::Keyboard;
use crate::perkey;

const ONBOARD: u8 = 0x01;
const HOST: u8 = 0x02;

/// Escape's lighting zone.
const ESCAPE: u8 = 0x26;

/// How long `--divert` leaves the keyboard alone: past the minute after
/// which the on-board effect draws over a painted key.
const IDLE: Duration = Duration::from_secs(90);

fn mode_name(m: u8) -> &'static str {
    match m {
        ONBOARD => "on-board",
        HOST => "host",
        _ => "unknown",
    }
}

fn read_mode(ch: &mut Channel, dev: u8, o: u8) -> Result<u8> {
    let p = ch.request(dev, o, 2, &[])?;
    Ok(p.first().copied().unwrap_or(0))
}

fn set_mode(ch: &mut Channel, dev: u8, o: u8, mode: u8, out: &mut Tee) -> Result<u8> {
    ch.request(dev, o, 1, &[mode])?;
    let now = read_mode(ch, dev, o)?;
    out.line(&format!("  mode set to {}: reads back {now} ({})", mode_name(mode), mode_name(now)));
    out.flush();
    Ok(now)
}

fn line() -> String {
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    s.trim().to_owned()
}

/// Ask for the M lights in `mask` (bit 0 = M1).
fn m_light(ch: &mut Channel, dev: u8, m: Option<u8>, mask: u8, name: &str, out: &mut Tee) {
    match m {
        Some(m) => match ch.request(dev, m, 1, &[mask]) {
            Ok(r) => {
                out.line(&format!("  {name} light asked for: reply {}", hex(&r[..3.min(r.len())])))
            }
            Err(e) => out.line(&format!("  {name} light refused: {e}")),
        },
        None => out.line("  no MKEYS feature"),
    }
    out.flush();
}

/// Take (`[01]`) or hand back (`[00]`) the G-keys.
fn gkeys(ch: &mut Channel, dev: u8, g: Option<u8>, take: bool, out: &mut Tee) {
    let Some(g) = g else {
        out.line("  no GKEY feature");
        return;
    };
    let what = if take { "taken" } else { "handed back" };
    match ch.request(dev, g, 2, &[take as u8]) {
        Ok(r) => out.line(&format!("  G-keys {what}: reply {}", hex(&r[..3.min(r.len())]))),
        Err(e) => out.line(&format!("  G-keys not {what}: {e}")),
    }
    out.flush();
}

pub fn run(restore: bool, leave: bool, divert: bool) -> Result<()> {
    let _lock = hidpp::acquire_lock()?;
    let mut out = Tee::create("host-test");
    out.line(&format!("BluGHub-diag host-test {} at {}", crate::version(), now_stamp()));
    let mut kb = Keyboard::open()?;
    let (ch, dev) = kb.channel();
    let index = |ch: &mut Channel, id: u16| -> Result<Option<u8>> {
        Ok(ch.feature_index(dev, id)?.map(|f| f.0))
    };
    let Some(o) = index(ch, feature::ONBOARD_PROFILES)? else {
        bail!("the keyboard has no ONBOARD_PROFILES feature")
    };
    let g = index(ch, feature::GKEY)?;
    let m = index(ch, feature::MKEYS)?;
    let mode = read_mode(ch, dev, o)?;
    out.line(&format!("  mode now: {mode} ({})", mode_name(mode)));
    out.flush();

    if restore {
        // In case a --divert run was cut short; harmless otherwise.
        gkeys(ch, dev, g, false, &mut out);
        if mode == ONBOARD {
            out.line("  already in on-board mode");
        } else if set_mode(ch, dev, o, ONBOARD, &mut out)? != ONBOARD {
            out.line("  STILL NOT IN ON-BOARD MODE: power-cycle the keyboard and run this again");
        }
        out.line(&format!("Written to {}", out.path.display()));
        out.flush();
        return Ok(());
    }
    if mode != ONBOARD {
        bail!("the keyboard is not in on-board mode; run `BluGHub-diag host-test --restore` first");
    }

    out.line("");
    out.line("  This switches the keyboard to HOST mode: its stored lighting, G-keys and M keys");
    out.line("  stop running until it is switched back (this test does that on Enter).");
    out.line("  If this window is closed or Ctrl+C'd before then, run");
    out.line("      BluGHub-diag host-test --restore");
    out.line("");
    out.console("  Type HOST and press Enter to switch (anything else stops): ");
    out.flush();
    let answer = line();
    out.file_only(&format!("  answer: {answer:?}"));
    if answer != "HOST" {
        out.line("  not switched; nothing sent");
        out.flush();
        return Ok(());
    }

    let now = match set_mode(ch, dev, o, HOST, &mut out) {
        Ok(m) => m,
        Err(e) => {
            out.line(&format!("  switching failed: {e}; making sure it is on-board"));
            let _ = set_mode(ch, dev, o, ONBOARD, &mut out);
            out.flush();
            return Err(e);
        }
    };
    if now != HOST {
        out.line("  the keyboard did not take host mode; nothing else sent");
        if now != ONBOARD {
            let _ = set_mode(ch, dev, o, ONBOARD, &mut out);
        }
        out.flush();
        return Ok(());
    }

    if divert {
        gkeys(ch, dev, g, true, &mut out);
    }
    // The M1 light, which on-board mode does not show.
    m_light(ch, dev, m, 0x01, "M1", &mut out);
    // Escape red, in the keyboard's RAM only.
    match index(ch, feature::PER_KEY_LIGHTING_V2)? {
        Some(pk) => {
            let mut ok = true;
            for (func, params) in perkey::requests(&[(ESCAPE, Rgb([0xFF, 0, 0]))]) {
                if let Err(e) = ch.request(dev, pk, func, &params) {
                    out.line(&format!("  painting Escape refused: {e}"));
                    ok = false;
                    break;
                }
            }
            if ok {
                match ch.request(dev, pk, perkey::FRAME_END, &perkey::FRAME_END_RAM) {
                    Ok(_) => out.line("  Escape painted red (RAM only)"),
                    Err(e) => out.line(&format!("  frame end refused: {e}")),
                }
            }
        }
        None => out.line("  no PER_KEY_LIGHTING_V2 feature"),
    }
    if divert {
        out.line("");
        out.line("  Is M1 lit? Is Escape red? Now leave the keyboard completely alone (no keys,");
        out.line(&format!("  no roller) for {} s and watch them.", IDLE.as_secs()));
        out.flush();
        let mut left = IDLE;
        while !left.is_zero() {
            let step = left.min(Duration::from_secs(15));
            std::thread::sleep(step);
            left -= step;
            out.line(&format!("    {} s left", left.as_secs()));
            out.flush();
        }
        out.line("  Time. Is Escape still red, and M1 still lit? Now asking for M3 instead:");
        let (ch, dev) = kb.channel();
        m_light(ch, dev, m, 0x04, "M3", &mut out);
    }
    out.line("");
    out.line("  Now look and try, and note what you see:");
    out.line("    - the lighting: everything, and Escape (red?)");
    out.line("    - the M keys: is M1 lit? Do M1-M3 still switch anything?");
    out.line("    - typing in Notepad, the F-row (F5 inserts the time), the media keys,");
    out.line("      the volume roller, and a G-key or two");
    out.flush();

    if leave {
        out.line("");
        out.line("  --leave: the keyboard STAYS in host mode. Switch it off and on, then run");
        out.line("      BluGHub-diag host-test --restore");
        out.line("  which first says which mode it came back in, then puts it on-board.");
        out.line(&format!("Written to {}", out.path.display()));
        out.flush();
        return Ok(());
    }

    out.console("  Press Enter to switch back to on-board mode: ");
    out.flush();
    let _ = line();
    let (ch, dev) = kb.channel();
    if divert {
        gkeys(ch, dev, g, false, &mut out);
    }
    match set_mode(ch, dev, o, ONBOARD, &mut out) {
        Ok(ONBOARD) => out.line("  back in on-board mode"),
        Ok(_) | Err(_) => out.line(
            "  NOT BACK IN ON-BOARD MODE: run `BluGHub-diag host-test --restore`, or power-cycle \
             the keyboard and run it",
        ),
    }
    out.line(&format!("Written to {}", out.path.display()));
    out.flush();
    Ok(())
}
