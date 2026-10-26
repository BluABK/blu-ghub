//! `apo-eq`: the equaliser of Logitech's audio processing on the G935's
//! playback endpoint: what Windows actually plays the headset through
//! (see [`crate::audio_fx`]). Nothing is sent to the headset.
//!
//!   apo-eq                       show it
//!   apo-eq set G1 ... G10        set the 10 gains (dB), after saving what is there
//!   apo-eq restore FILE          put a saved one back
//!
//! The changes go through Windows' audio policy service, as the Sound
//! control panel's effect settings do: no administrator rights.

use anyhow::{Context, Result, anyhow, bail};

use super::{Tee, hex, now_stamp};
use crate::audio_fx::{self, LOGI_EQ, LogiEq};

pub fn run(args: &[&str]) -> Result<()> {
    let _com = crate::audio::Com::init()?;
    let mut out = Tee::create("apo-eq");
    out.line(&format!("BluGHub-diag apo-eq {} at {}", crate::version(), now_stamp()));
    let all = crate::hid::logitech_collections()?;
    let path = crate::hidpp::hidpp_groups(&all, 0x0A87)
        .first()
        .and_then(|g| g.first())
        .map(|c| c.path.clone())
        .ok_or_else(|| anyhow!("no G935 dongle found"))?;
    let container = crate::audio::container_of_interface(&path)?;
    let endpoints = audio_fx::render_endpoints(container)?;
    if endpoints.is_empty() {
        bail!("Windows lists no playback endpoint for the G935");
    }
    for (n, ep) in endpoints.iter().enumerate() {
        out.line(&format!("  endpoint {n}: {ep}"));
        show(&mut out, ep);
    }

    match args {
        [] => {}
        ["set", gains @ ..] => {
            if gains.len() != 10 {
                bail!("give 10 gains in dB, 32 Hz-ish band first (got {})", gains.len());
            }
            let gains: Vec<f32> = gains
                .iter()
                .map(|g| g.parse::<f32>().map_err(|_| anyhow!("not a number: {g}")))
                .collect::<Result<_>>()?;
            if gains.iter().any(|g| !(-24.0..=24.0).contains(g)) {
                bail!("keep the gains within -24 to +24 dB");
            }
            for (n, ep) in endpoints.iter().enumerate() {
                let before = audio_fx::get_fx(ep, LOGI_EQ)?;
                let saved = crate::app_paths::diag_dir()
                    .join(format!("apo-eq-{}-{n}.bin", now_stamp().replace([':', ' '], "-")));
                std::fs::write(&saved, &before)
                    .with_context(|| format!("saving {}", saved.display()))?;
                out.line(&format!("  endpoint {n}: saved the equaliser to {}", saved.display()));
                let mut eq = LogiEq::parse(&before)?;
                eq.gains.copy_from_slice(&gains);
                audio_fx::set_fx(ep, LOGI_EQ, &eq.to_bytes())?;
                out.line(&format!("  endpoint {n}: gains set; reads back:"));
                show(&mut out, ep);
            }
            out.line("  Listen: does the sound change? `apo-eq restore FILE` puts it back.");
        }
        ["restore", file] => {
            let bytes = std::fs::read(file).with_context(|| format!("reading {file}"))?;
            LogiEq::parse(&bytes)?;
            for (n, ep) in endpoints.iter().enumerate() {
                audio_fx::set_fx(ep, LOGI_EQ, &bytes)?;
                out.line(&format!("  endpoint {n}: put back; reads back:"));
                show(&mut out, ep);
            }
        }
        _ => bail!("usage: apo-eq [set G1 ... G10 | restore FILE]"),
    }
    out.line(&format!("Written to {}", out.path.display()));
    out.flush();
    Ok(())
}

fn show(out: &mut Tee, endpoint: &str) {
    match audio_fx::get_fx(endpoint, LOGI_EQ) {
        Ok(b) => match LogiEq::parse(&b) {
            Ok(eq) => {
                out.line(&format!(
                    "    {} bytes after the bands: {}",
                    eq.rest.len(),
                    hex(&eq.rest)
                ));
                for i in 0..10 {
                    out.line(&format!(
                        "    band {i}: {:>7.0} Hz  {:+5.1} dB  Q {:.2}",
                        eq.freqs[i], eq.gains[i], eq.q[i]
                    ));
                }
            }
            Err(e) => out.line(&format!("    {e}: {}", hex(&b))),
        },
        Err(e) => out.line(&format!("    no Logitech equaliser here: {e:#}")),
    }
    out.flush();
}
