//! `sniff`: record every HID++ report the receiver and the headset send to
//! the PC, without sending anything.
//!
//! Windows gives each open handle its own copy of every input report, so a
//! read-only handle sees the devices' notifications *and* the replies to
//! whatever any other program (G HUB, the LampArray service, OpenRGB) asks of
//! them. That answers, before blu-ghub writes a single byte: is anything else
//! talking to these devices, with which software id, and do battery and
//! connection events arrive without being asked for?

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use super::{STOP, Tee, hex, install_stop_handler, now_stamp, stamp};
use crate::hid::{self, HidInfo, ReaderEnd, StopEvent};

/// A HID++ report, decoded as far as is possible without knowing the
/// device's feature table (which maps feature *indices* to feature ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decoded {
    /// Not a HID++ report id.
    Other { report_id: u8 },
    /// Too short to decode.
    Short,
    /// HID++ 1.0 error reply (`8F`).
    Error10 { dev: u8, sub_id: u8, address: u8, code: u8 },
    /// HID++ 2.0 error reply (`FF`).
    Error20 { dev: u8, feature_index: u8, function: u8, sw_id: u8, code: u8 },
    /// HID++ 1.0 notification from the receiver (sub-id 0x40..=0x7F),
    /// e.g. 0x41 "device connection".
    Notification10 { dev: u8, sub_id: u8, params: [u8; 4] },
    /// HID++ 1.0 register access reply (0x80 set / 0x81 get short register,
    /// 0x82 set / 0x83 get long register).
    Register { dev: u8, sub_id: u8, register: u8 },
    /// HID++ 2.0 message: a reply when `sw_id` is non-zero (it echoes the
    /// requester's id), an unsolicited event when it is zero.
    Feature { dev: u8, feature_index: u8, function: u8, sw_id: u8 },
}

pub fn decode(d: &[u8]) -> Decoded {
    let Some(&report_id) = d.first() else { return Decoded::Short };
    if !matches!(report_id, 0x10..=0x12) {
        return Decoded::Other { report_id };
    }
    if d.len() < 7 {
        return Decoded::Short;
    }
    let (dev, sub) = (d[1], d[2]);
    match sub {
        0x8F => Decoded::Error10 { dev, sub_id: d[3], address: d[4], code: d[5] },
        0xFF => Decoded::Error20 {
            dev,
            feature_index: d[3],
            function: d[4] >> 4,
            sw_id: d[4] & 0x0F,
            code: d[5],
        },
        0x40..=0x7F => {
            Decoded::Notification10 { dev, sub_id: sub, params: [d[3], d[4], d[5], d[6]] }
        }
        0x80..=0x83 => Decoded::Register { dev, sub_id: sub, register: d[3] },
        _ => Decoded::Feature { dev, feature_index: sub, function: d[3] >> 4, sw_id: d[3] & 0x0F },
    }
}

pub fn error10_name(code: u8) -> &'static str {
    match code {
        0x01 => "invalid sub-id",
        0x02 => "invalid address",
        0x03 => "invalid value",
        0x04 => "connect fail",
        0x05 => "too many devices",
        0x06 => "already exists",
        0x07 => "busy",
        0x08 => "unknown device",
        0x09 => "resource error (device asleep/offline)",
        0x0A => "request unavailable",
        0x0B => "invalid parameter value",
        0x0C => "wrong PIN",
        _ => "?",
    }
}

pub fn error20_name(code: u8) -> &'static str {
    match code {
        0x01 => "unknown",
        0x02 => "invalid argument",
        0x03 => "out of range",
        0x04 => "hardware error",
        0x05 => "not allowed",
        0x06 => "invalid feature index",
        0x07 => "invalid function",
        0x08 => "busy",
        0x09 => "unsupported",
        _ => "?",
    }
}

impl std::fmt::Display for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Decoded::Other { report_id } => write!(f, "report id {report_id:02X} (not HID++)"),
            Decoded::Short => write!(f, "truncated"),
            Decoded::Error10 { dev, sub_id, address, code } => write!(
                f,
                "dev {dev:02X} HID++1.0 ERROR {code:02X} {} (to sub {sub_id:02X} addr {address:02X})",
                error10_name(code)
            ),
            Decoded::Error20 { dev, feature_index, function, sw_id, code } => write!(
                f,
                "dev {dev:02X} HID++2.0 ERROR {code:02X} {} (to feat#{feature_index:02X} fn{function} sw{sw_id:X})",
                error20_name(code)
            ),
            Decoded::Notification10 { dev, sub_id, params } => {
                let what = match sub_id {
                    0x40 => "device disconnection".to_string(),
                    // [protocol, flags, wpid lo, wpid hi]; flag bit 6 = no link.
                    0x41 => {
                        let link = if params[1] & 0x40 != 0 { "link DOWN" } else { "link up" };
                        format!(
                            "device connection, {link}, wpid {:02X}{:02X}",
                            params[3], params[2]
                        )
                    }
                    0x4A => "lock/pairing status".to_string(),
                    _ => format!("notification {sub_id:02X}"),
                };
                write!(f, "dev {dev:02X} HID++1.0 {what} [{}]", hex(&params))
            }
            Decoded::Register { dev, sub_id, register } => {
                let op = match sub_id {
                    0x80 => "set short",
                    0x81 => "get short",
                    0x82 => "set long",
                    _ => "get long",
                };
                write!(f, "dev {dev:02X} register reply: {op} reg {register:02X}")
            }
            Decoded::Feature { dev, feature_index, function, sw_id } => {
                if sw_id == 0 {
                    write!(f, "dev {dev:02X} EVENT feat#{feature_index:02X} fn{function}")
                } else {
                    write!(
                        f,
                        "dev {dev:02X} reply feat#{feature_index:02X} fn{function} to sw{sw_id:X}"
                    )
                }
            }
        }
    }
}

/// Which collections to listen on: the HID++ channels of the LIGHTSPEED
/// receiver (and of the G915 itself when it is on its cable), and the
/// headset's HID++ collection. Never the headset's other vendor collection.
fn hidpp_endpoints(all: &[HidInfo]) -> Vec<HidInfo> {
    all.iter()
        .filter(|i| super::caps::role(i).starts_with("HID++"))
        .filter(|i| i.input_len > 0)
        .cloned()
        .collect()
}

/// The counting key for the summary: who sent what kind of message.
fn class(d: &Decoded) -> String {
    match *d {
        Decoded::Other { report_id } => format!("report {report_id:02X}"),
        Decoded::Short => "truncated".into(),
        Decoded::Error10 { dev, code, .. } => format!("dev {dev:02X} 1.0 error {code:02X}"),
        Decoded::Error20 { dev, feature_index, sw_id, code, .. } => {
            format!("dev {dev:02X} 2.0 error {code:02X} feat#{feature_index:02X} sw{sw_id:X}")
        }
        Decoded::Notification10 { dev, sub_id, .. } => {
            format!("dev {dev:02X} 1.0 notif {sub_id:02X}")
        }
        Decoded::Register { dev, sub_id, register } => {
            format!("dev {dev:02X} register {sub_id:02X} reg {register:02X}")
        }
        Decoded::Feature { dev, feature_index, sw_id: 0, function } => {
            format!("dev {dev:02X} event feat#{feature_index:02X} fn{function}")
        }
        Decoded::Feature { dev, feature_index, sw_id, .. } => {
            format!("dev {dev:02X} reply feat#{feature_index:02X} sw{sw_id:X}")
        }
    }
}

pub fn run(minutes: Option<u64>) -> Result<()> {
    let mut out = Tee::create("sniff");
    out.line(&format!("BluGHub-diag sniff {} at {}", crate::version(), now_stamp()));
    out.line("Read-only: nothing is sent to any device. Ctrl+C stops and prints a summary.");

    let endpoints = hidpp_endpoints(&hid::logitech_collections()?);
    if endpoints.is_empty() {
        bail!("no HID++ collections found (is the receiver or headset plugged in?)");
    }

    let stop = StopEvent::new()?;
    install_stop_handler(Some(stop.raw()), None);
    let stop_raw = stop.raw().0 as isize;

    let (tx, rx) = mpsc::channel();
    let mut readers = Vec::new();
    for (n, ep) in endpoints.iter().enumerate() {
        let handle = match hid::open_read(&ep.path) {
            Ok(h) => h,
            Err(e) => {
                out.line(&format!("  [{n}] {}: cannot open for reading: {e:#}", ep.short()));
                continue;
            }
        };
        out.line(&format!(
            "  [{n}] {} {} (in {} bytes)",
            ep.short(),
            super::caps::role(ep),
            ep.input_len
        ));
        let tx = tx.clone();
        let len = ep.input_len as usize;
        let label = ep.short();
        readers.push(thread::Builder::new().name(format!("sniff-{n}")).spawn(move || {
            let stop = windows::Win32::Foundation::HANDLE(stop_raw as *mut _);
            match hid::read_reports(n, &handle, len, stop, &tx) {
                ReaderEnd::Stopped => None,
                ReaderEnd::Gone(why) => Some(format!("{label}: reader ended: {why}")),
            }
        })?);
    }
    drop(tx);
    if readers.is_empty() {
        bail!("could not open any HID++ collection");
    }

    let started = Instant::now();
    let deadline = minutes.map(|m| started + Duration::from_secs(m * 60));
    let mut next_summary = started + Duration::from_secs(3600);
    let mut counts: BTreeMap<(usize, String), u64> = BTreeMap::new();
    let mut total = 0u64;

    loop {
        if STOP.load(Ordering::SeqCst) || deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(r) => {
                total += 1;
                // Stamp with the moment the reader got the report, not the
                // moment this loop printed it: a busy console must not
                // squeeze a gap between two events into nothing.
                let lag = chrono::Duration::from_std(r.at.elapsed()).unwrap_or_default();
                let when = stamp(chrono::Local::now() - lag);
                let d = decode(&r.data);
                out.line(&format!(
                    "{when} [{}] {:<44} {}",
                    r.endpoint,
                    d.to_string(),
                    hex(&r.data)
                ));
                *counts.entry((r.endpoint, class(&d))).or_default() += 1;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                out.line("All readers have ended.");
                break;
            }
        }
        let now = Instant::now();
        if now >= next_summary {
            summary(&mut out, &endpoints, &counts, total, started.elapsed());
            // From now, not from the old target: one summary after a long
            // stall or a sleep, not one per missed hour.
            next_summary = now + Duration::from_secs(3600);
        }
    }

    stop.set();
    for r in readers {
        if let Ok(Some(why)) = r.join() {
            out.line(&why);
        }
    }
    summary(&mut out, &endpoints, &counts, total, started.elapsed());
    out.line(&format!("Written to {}", out.path.display()));
    out.flush();
    Ok(())
}

fn summary(
    out: &mut Tee,
    endpoints: &[HidInfo],
    counts: &BTreeMap<(usize, String), u64>,
    total: u64,
    elapsed: Duration,
) {
    out.line(&format!(
        "--- summary after {:.1} min: {total} reports ---",
        elapsed.as_secs_f64() / 60.0
    ));
    for ((ep, class), n) in counts {
        out.line(&format!("  [{ep}] {:<26} {class:<44} {n}", endpoints[*ep].short()));
    }
    out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_carries_the_requesters_software_id() {
        // Long report: device 1, feature index 0x0C, function 1, sw id 9.
        let r = [0x11, 0x01, 0x0C, 0x19, 0x00, 0x00, 0x00];
        assert_eq!(
            decode(&r),
            Decoded::Feature { dev: 1, feature_index: 0x0C, function: 1, sw_id: 9 }
        );
    }

    #[test]
    fn software_id_zero_is_an_unsolicited_event() {
        let r = [0x11, 0xFF, 0x08, 0x00, 0x0F, 0xA4, 0x01];
        assert!(decode(&r).to_string().contains("EVENT"));
    }

    #[test]
    fn both_error_forms_are_recognised() {
        let e10 = [0x10, 0x01, 0x8F, 0x0C, 0x19, 0x09, 0x00];
        assert_eq!(
            decode(&e10),
            Decoded::Error10 { dev: 1, sub_id: 0x0C, address: 0x19, code: 0x09 }
        );
        let e20 = [0x11, 0xFF, 0xFF, 0x04, 0x39, 0x07, 0x00];
        assert_eq!(
            decode(&e20),
            Decoded::Error20 { dev: 0xFF, feature_index: 4, function: 3, sw_id: 9, code: 7 }
        );
    }

    #[test]
    fn a_link_down_connection_notification_says_so() {
        let n = [0x10, 0x01, 0x41, 0x04, 0x41, 0x7C, 0x40];
        assert!(decode(&n).to_string().contains("link DOWN"));
    }

    #[test]
    fn keyboard_reports_are_not_mistaken_for_hidpp() {
        assert_eq!(decode(&[0x01, 0, 0, 0]), Decoded::Other { report_id: 1 });
    }
}
