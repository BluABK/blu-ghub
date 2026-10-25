//! BluGHub-diag: evidence gathering for blu-ghub. Console program.
//!
//!   BluGHub-diag caps                 list Logitech HID collections and report sizes
//!   BluGHub-diag keys  [--minutes N]  key-timing logger (lost/late key-ups, chatter)
//!   BluGHub-diag sniff [--minutes N]  passive capture of HID++ traffic
//!   BluGHub-diag probe                what the devices are and support (read requests)
//!   BluGHub-diag dump-onboard         back up the keyboard's on-board memory (reads)
//!
//! caps, keys and sniff send nothing to a device; probe and dump-onboard send
//! HID++ read requests only. Output also goes to %APPDATA%\BluGHub\diag\.

use anyhow::{Result, bail};

fn usage() -> &'static str {
    "usage:\n  BluGHub-diag caps\n  BluGHub-diag keys  [--minutes N]\n  BluGHub-diag sniff [--minutes N]\n  \
     BluGHub-diag probe\n  BluGHub-diag dump-onboard\n\n\
     caps, keys and sniff send nothing to any device; probe and dump-onboard send\n\
     read requests only. Logs go to %APPDATA%\\BluGHub\\diag\\."
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut minutes = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minutes" | "-m" => {
                let Some(v) = it.next() else { bail!("--minutes needs a number\n\n{}", usage()) };
                minutes =
                    Some(v.parse::<u64>().map_err(|_| anyhow::anyhow!("bad --minutes: {v}"))?);
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(());
            }
            _ => rest.push(a.as_str()),
        }
    }
    match rest.as_slice() {
        ["caps"] => blu_ghub::diag::caps::run(),
        ["probe"] => blu_ghub::diag::probe::run(),
        ["dump-onboard"] => blu_ghub::diag::probe::dump_onboard(),
        ["keys"] => blu_ghub::diag::keys::run(minutes),
        ["sniff"] => blu_ghub::diag::sniff::run(minutes),
        _ => bail!("{}", usage()),
    }
}
