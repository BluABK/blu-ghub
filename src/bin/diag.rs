//! BluGHub-diag: evidence gathering for blu-ghub. Console program.
//!
//!   BluGHub-diag caps                 list Logitech HID collections and report sizes
//!   BluGHub-diag keys  [--minutes N]  key-timing logger (lost/late key-ups, chatter)
//!   BluGHub-diag sniff [--minutes N] [--headset-all]  passive capture of HID++ traffic
//!   BluGHub-diag probe                what the devices are and support (read requests)
//!   BluGHub-diag dump-onboard         back up the keyboard's on-board memory (reads)
//!   BluGHub-diag slots                the keyboard's stored-lighting slots (reads)
//!
//! caps, keys and sniff send nothing to a device; probe, dump-onboard and slots send
//! HID++ read requests only. Output also goes to %APPDATA%\BluGHub\diag\.

use anyhow::{Result, bail};

fn usage() -> &'static str {
    "usage:\n  BluGHub-diag caps\n  BluGHub-diag keys  [--minutes N] [--echo]\n  BluGHub-diag sniff [--minutes N] [--headset-all]\n  \
     BluGHub-diag probe\n  BluGHub-diag dump-onboard\n  BluGHub-diag slots\n\n\
     Writing the keyboard's on-board memory (each saves a backup first and\n\
     verifies the result):\n  \
     BluGHub-diag write-test SECTOR          write a profile back unchanged\n  \
     BluGHub-diag set-gkeys SECTOR G3=F15 G5=LCtrl G1=off ...\n  \
     BluGHub-diag restore-sector SECTOR FILE put a saved sector back\n\n\
     Lighting (nothing stored; a power cycle undoes it):\n  \
     BluGHub-diag light-test [--claim]       paint Escape red over the stored layout\n\n\
     The keyboard's mode (asks before switching; switches back on Enter):\n  \
     BluGHub-diag host-test [--leave]        try host mode, then back to on-board\n  \
     BluGHub-diag host-test --restore        put the keyboard back in on-board mode\n\n\
     caps, keys and sniff send nothing to any device; probe, dump-onboard and slots send\n\
     read requests only; only the commands under the last three headings write.\n\
     Logs go to %APPDATA%\\BluGHub\\diag\\."
}

fn parse_sector(s: &str) -> Result<u16> {
    let v = match s.strip_prefix("0x") {
        Some(h) => u16::from_str_radix(h, 16),
        None => s.parse(),
    };
    v.map_err(|_| anyhow::anyhow!("bad sector number: {s}"))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut minutes = None;
    let mut echo = false;
    let mut headset_all = false;
    let mut claim = false;
    let mut restore = false;
    let mut leave = false;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minutes" | "-m" => {
                let Some(v) = it.next() else { bail!("--minutes needs a number\n\n{}", usage()) };
                minutes =
                    Some(v.parse::<u64>().map_err(|_| anyhow::anyhow!("bad --minutes: {v}"))?);
            }
            "--echo" => echo = true,
            "--headset-all" => headset_all = true,
            "--claim" => claim = true,
            "--restore" => restore = true,
            "--leave" => leave = true,
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
        ["dump-onboard"] => blu_ghub::diag::onboard::dump(),
        ["write-test", sector] => blu_ghub::diag::onboard::write_test(parse_sector(sector)?),
        ["set-gkeys", sector, assignments @ ..] => {
            blu_ghub::diag::onboard::set_gkeys(parse_sector(sector)?, assignments)
        }
        ["restore-sector", sector, file] => {
            blu_ghub::diag::onboard::restore(parse_sector(sector)?, std::path::Path::new(file))
        }
        ["keys"] => blu_ghub::diag::keys::run(minutes, echo),
        ["sniff"] => blu_ghub::diag::sniff::run(minutes, headset_all),
        ["light-test"] => blu_ghub::diag::light::run(claim),
        ["slots"] => blu_ghub::diag::slots::run(),
        ["host-test"] if restore && leave => bail!("--restore and --leave do not go together"),
        ["host-test"] => blu_ghub::diag::host::run(restore, leave),
        _ => bail!("{}", usage()),
    }
}
