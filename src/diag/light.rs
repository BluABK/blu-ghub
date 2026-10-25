//! `light-test`: experiment E7. Can blu-ghub paint keys over the G915's
//! stored per-key layout without changing anything stored?
//!
//! Paints Escape red as a frame held only in the keyboard's memory: 0x8081
//! (PER_KEY_LIGHTING_V2) fn1, then fn7 "frame end" with its persistence
//! byte 0 (RAM only; 1 would write the frame to the keyboard's own
//! memory, which is never sent). The on-board mode (0x8100) is not
//! touched. A power cycle, an M key, or a minute idle brings the stored
//! layout back.
//!
//! With `--claim` it first takes the lighting over from the keyboard's own
//! effect engine (0x8071 fn5 set, clusters only, which the keyboard forgets
//! on power-off), paints, waits for Enter, and hands it back.

use anyhow::{Result, bail};

use super::{Tee, hex, now_stamp};
use crate::hidpp::{self, HidppError, feature};
use crate::onboard::Keyboard;

/// Escape's lighting zone: HID usage 0x29 - 3.
const ESCAPE: u8 = 0x26;

pub fn run(claim: bool) -> Result<()> {
    let _lock = hidpp::acquire_lock()?;
    let mut out = Tee::create("light-test");
    out.line(&format!("BluGHub-diag light-test {} at {}", crate::version(), now_stamp()));
    let mut kb = Keyboard::open()?;
    let (ch, dev) = kb.channel();
    let index = |ch: &mut hidpp::Channel, id: u16, name: &str| -> Result<u8> {
        match ch.feature_index(dev, id)? {
            Some((i, _, _)) => Ok(i),
            None => bail!("the keyboard has no {name} feature"),
        }
    };
    let effects = index(ch, feature::RGB_EFFECTS, "RGB_EFFECTS")?;
    let per_key = index(ch, feature::PER_KEY_LIGHTING_V2, "PER_KEY_LIGHTING_V2")?;
    out.line(&format!("  RGB_EFFECTS at {effects:#04x}, PER_KEY_LIGHTING_V2 at {per_key:#04x}"));

    // Read-only: who drives the lighting now (00 = the keyboard itself).
    let control = ch.request(dev, effects, 5, &[0x00, 0x00, 0x00])?;
    out.line(&format!("  software control now: {}", hex(&control[..3.min(control.len())])));

    let mut step =
        |out: &mut Tee, what: &str, feat: u8, func: u8, params: &[u8]| -> Result<Vec<u8>> {
            match ch.request(dev, feat, func, params) {
                Ok(r) => {
                    out.line(&format!("  {what}: reply {}", hex(&r[..r.len().min(16)])));
                    out.flush();
                    Ok(r)
                }
                Err(e @ HidppError::Error20(_)) | Err(e @ HidppError::Error10(_)) => {
                    out.line(&format!("  {what}: refused ({e}); stopping"));
                    out.flush();
                    bail!("{what}: {e}")
                }
                Err(e) => bail!("{what}: {e}"),
            }
        };

    if claim {
        step(&mut out, "take lighting control (clusters only)", effects, 5, &[0x01, 0x01, 0x00])?;
    }
    // Twice under a fresh claim: the first frame after one can be dropped.
    for _ in 0..if claim { 2 } else { 1 } {
        // One zone, red; FF ends the list.
        step(&mut out, "Escape red", per_key, 1, &[ESCAPE, 0xFF, 0x00, 0x00, 0xFF])?;
        // Frame end, persistence 0: RAM only.
        step(&mut out, "show frame (RAM only)", per_key, 7, &[0x00, 0x00, 0x00, 0x00, 0x00])?;
    }
    out.line("");
    out.line("  Look at the keyboard: is Escape red, and is everything else as it was?");
    if claim {
        out.line("  Press Enter to hand the lighting back to the keyboard.");
        out.flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        let (ch, dev) = kb.channel();
        match ch.request(dev, effects, 5, &[0x01, 0x00, 0x00]) {
            Ok(r) => {
                out.line(&format!("  lighting handed back: reply {}", hex(&r[..3.min(r.len())])))
            }
            Err(e) => out.line(&format!("  could not hand back ({e}): power-cycle the keyboard")),
        }
    } else {
        out.line("  It should go back to your layout by itself after a minute idle, on an M");
        out.line("  key, or when the keyboard is switched off and on.");
    }
    out.line(&format!("Written to {}", out.path.display()));
    out.flush();
    Ok(())
}
