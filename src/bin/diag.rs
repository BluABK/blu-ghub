//! BluGHub-diag: evidence gathering for blu-ghub. Console program.
//!
//!   BluGHub-diag caps                 list Logitech HID collections and report sizes
//!   BluGHub-diag keys  [--minutes N]  key-timing logger (lost/late key-ups, chatter)
//!   BluGHub-diag sniff [--minutes N]  passive capture of HID++ traffic
//!
//! None of these send anything to a device. Output also goes to
//! %APPDATA%\BluGHub\diag\.

use anyhow::{Result, bail};

fn usage() -> &'static str {
    "usage:\n  BluGHub-diag caps\n  BluGHub-diag keys  [--minutes N]\n  BluGHub-diag sniff [--minutes N]\n\n\
     Nothing is sent to any device. Logs go to %APPDATA%\\BluGHub\\diag\\."
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
        ["keys"] => {
            let _quick_edit = blu_ghub::diag::disable_quick_edit();
            blu_ghub::diag::keys::run(minutes)
        }
        ["sniff"] => {
            let _quick_edit = blu_ghub::diag::disable_quick_edit();
            blu_ghub::diag::sniff::run(minutes)
        }
        _ => bail!("{}", usage()),
    }
}
