//! HID++ — Logitech's protocol for talking to its devices over vendor-defined
//! HID reports.
//!
//! Three report sizes: short (id 0x10, 7 bytes), long (0x11, 20 bytes) and
//! very long (0x12, 64 bytes, not used here). Each lives in its own HID
//! collection on Windows, so the receiver is two handles (short and long) and
//! the G935 dongle one (it has no short reports at all).
//!
//! Two generations share the transport:
//! - **HID++ 1.0**: register reads and writes, used by the receiver itself
//!   (device index 0xFF) — `sub_id` 0x80/0x81 short register, 0x82/0x83 long.
//! - **HID++ 2.0**: features, used by the keyboard and headset. A device
//!   lists its features in a table; a request names a feature by its *index*
//!   in that table (looked up through feature 0, ROOT), a function number and
//!   a software id. The reply echoes all three.
//!
//! [`Channel`] keeps one request in flight and matches replies on (device,
//! feature or sub-id, function, software id). Everything else that arrives —
//! device notifications, and replies to *other* programs talking to the same
//! device — is queued for the caller rather than mistaken for an answer.

use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

use crate::hid::{self, HidInfo, OwnedHandle, Report, StopEvent};

pub const SHORT: u8 = 0x10;
pub const LONG: u8 = 0x11;
pub const SHORT_LEN: usize = 7;
pub const LONG_LEN: usize = 20;

/// The receiver itself (and a device on its own cable or dongle).
pub const RECEIVER: u8 = 0xFF;

/// Our software id, echoed back in every HID++ 2.0 reply. Others in use:
/// 0x01 (OpenRGB, the kernel, possibly G HUB), 0x07, 0x0A-0x0F (Solaar,
/// HeadsetControl, LGSTrayEx, G HUB); 0 marks unsolicited events.
pub const SW_ID: u8 = 0x09;

/// Well-known HID++ 2.0 feature ids.
pub mod feature {
    pub const ROOT: u16 = 0x0000;
    pub const FEATURE_SET: u16 = 0x0001;
    pub const DEVICE_FW_VERSION: u16 = 0x0003;
    pub const DEVICE_NAME: u16 = 0x0005;
    pub const BATTERY_STATUS: u16 = 0x1000;
    pub const BATTERY_VOLTAGE: u16 = 0x1001;
    pub const UNIFIED_BATTERY: u16 = 0x1004;
    pub const WIRELESS_DEVICE_STATUS: u16 = 0x1D4B;
    pub const ADC_MEASUREMENT: u16 = 0x1F20;
    pub const GAME_MODE: u16 = 0x4522;
    pub const GKEY: u16 = 0x8010;
    pub const MKEYS: u16 = 0x8020;
    pub const MR: u16 = 0x8030;
    pub const BRIGHTNESS: u16 = 0x8040;
    pub const COLOR_LED_EFFECTS: u16 = 0x8070;
    pub const RGB_EFFECTS: u16 = 0x8071;
    pub const PER_KEY_LIGHTING: u16 = 0x8080;
    pub const PER_KEY_LIGHTING_V2: u16 = 0x8081;
    pub const ONBOARD_PROFILES: u16 = 0x8100;
    pub const SIDETONE: u16 = 0x8300;
    pub const EQUALIZER: u16 = 0x8310;

    pub fn name(id: u16) -> &'static str {
        match id {
            ROOT => "ROOT",
            FEATURE_SET => "FEATURE_SET",
            0x0002 => "FEATURE_INFO",
            DEVICE_FW_VERSION => "DEVICE_FW_VERSION",
            DEVICE_NAME => "DEVICE_NAME",
            0x0007 => "DEVICE_FRIENDLY_NAME",
            0x0020 => "CONFIG_CHANGE",
            0x00C2 => "DFU_CONTROL_SIGNED",
            0x00D0 => "DFU",
            BATTERY_STATUS => "BATTERY_STATUS",
            BATTERY_VOLTAGE => "BATTERY_VOLTAGE",
            UNIFIED_BATTERY => "UNIFIED_BATTERY",
            0x1814 => "CHANGE_HOST",
            0x1B04 => "REPROG_CONTROLS_V4",
            0x1BC0 => "REPORT_HID_USAGE",
            WIRELESS_DEVICE_STATUS => "WIRELESS_DEVICE_STATUS",
            0x1DF3 => "EQUALIZER (old)",
            ADC_MEASUREMENT => "ADC_MEASUREMENT",
            0x1E00 => "ENABLE_HIDDEN_FEATURES",
            0x1EB0 => "TF_FIRMWARE",
            0x18A1 => "LED_SW_CONTROL",
            0x4521 => "KEYBOARD_DISABLE_KEYS",
            GAME_MODE => "GAME_MODE / DISABLE_KEYS_BY_USAGE",
            0x4540 => "KEYBOARD_LAYOUT_2",
            GKEY => "GKEY",
            MKEYS => "MKEYS",
            MR => "MR",
            BRIGHTNESS => "BRIGHTNESS_CONTROL",
            COLOR_LED_EFFECTS => "COLOR_LED_EFFECTS",
            RGB_EFFECTS => "RGB_EFFECTS",
            PER_KEY_LIGHTING => "PER_KEY_LIGHTING",
            PER_KEY_LIGHTING_V2 => "PER_KEY_LIGHTING_V2",
            ONBOARD_PROFILES => "ONBOARD_PROFILES",
            0x8101 => "PROFILE_MANAGEMENT",
            SIDETONE => "SIDETONE",
            EQUALIZER => "EQUALIZER",
            _ => "?",
        }
    }
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidppError {
    /// No reply in time.
    Timeout,
    /// The receiver answered for the device: it is asleep, switched off or
    /// out of range (HID++ 1.0 error 0x09 "resource error", or 0x04).
    Offline(u8),
    /// Other HID++ 1.0 error (code).
    Error10(u8),
    /// HID++ 2.0 error (code): 0x05 not allowed, 0x07 invalid function,
    /// 0x08 busy, ...
    Error20(u8),
    /// The device or its handle went away.
    Io(String),
}

impl std::fmt::Display for HidppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::diag::sniff::{error10_name, error20_name};
        match self {
            HidppError::Timeout => write!(f, "no reply"),
            HidppError::Offline(c) => {
                write!(f, "device offline or asleep (HID++ 1.0 error {c:02X} {})", error10_name(*c))
            }
            HidppError::Error10(c) => write!(f, "HID++ 1.0 error {c:02X} {}", error10_name(*c)),
            HidppError::Error20(c) => write!(f, "HID++ 2.0 error {c:02X} {}", error20_name(*c)),
            HidppError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for HidppError {}

/// What a reply must look like to be ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    pub dev: u8,
    /// Feature index (2.0) or sub-id (1.0 register access).
    pub sub: u8,
    /// `fn << 4 | sw_id` (2.0) or the register address (1.0).
    pub addr: u8,
}

/// How a received report relates to a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// The reply; its payload (the bytes after the 4-byte header).
    Reply(Vec<u8>),
    Error10(u8),
    Error20(u8),
    /// Not about this request.
    Unrelated,
}

/// Classify `r` against the request described by `e`. Pure, for testing.
pub fn classify(r: &[u8], e: Expect) -> Match {
    if r.len() < SHORT_LEN || !matches!(r[0], SHORT | LONG) || r[1] != e.dev {
        return Match::Unrelated;
    }
    // HID++ 1.0 error: 8F <request sub/feature> <request addr/fn|sw> <code>.
    // The receiver also answers this way for a 2.0 request to a device that
    // is asleep.
    if r[2] == 0x8F && r[3] == e.sub && r[4] == e.addr {
        return Match::Error10(r[5]);
    }
    // HID++ 2.0 error: FF <feature index> <fn|sw> <code>.
    if r[2] == 0xFF && r[3] == e.sub && r[4] == e.addr {
        return Match::Error20(r[5]);
    }
    if r[2] == e.sub && r[3] == e.addr {
        return Match::Reply(r[4..].to_vec());
    }
    Match::Unrelated
}

/// A received report that was not a reply to us.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub at: Instant,
    pub data: Vec<u8>,
}

/// Holds the machine-wide "I am talking HID++ to these devices" lock, so two
/// blu-ghub processes (the tray app and a probe) never interleave requests.
pub struct HidppLock(HANDLE);

unsafe impl Send for HidppLock {}

impl Drop for HidppLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn acquire_lock() -> Result<HidppLock> {
    let h = unsafe { CreateMutexW(None, true, w!("Local\\BluGHub.hidpp")) }
        .context("creating the HID++ lock")?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            let _ = CloseHandle(h);
        }
        bail!("another blu-ghub process is talking to the devices (quit the tray app first)");
    }
    Ok(HidppLock(h))
}

struct Endpoint {
    handle: std::sync::Arc<OwnedHandle>,
    out_len: usize,
}

/// A conversation with one HID++ device (or the receiver and the devices
/// behind it): one or two collections, a reader thread on each.
pub struct Channel {
    short: Option<Endpoint>,
    long: Endpoint,
    rx: Receiver<Report>,
    stop: StopEvent,
    readers: Vec<JoinHandle<()>>,
    queued: VecDeque<Incoming>,
    pub label: String,
    pub timeout: Duration,
}

impl Channel {
    /// Open the HID++ channel(s) among `collections`: the long collection is
    /// required, the short one optional (the G935 has none).
    pub fn open(collections: &[&HidInfo], label: &str) -> Result<Channel> {
        let long_info = collections
            .iter()
            .find(|i| i.output_len as usize == LONG_LEN)
            .ok_or_else(|| anyhow!("{label}: no long (20-byte) HID++ collection"))?;
        let short_info = collections.iter().find(|i| i.output_len as usize == SHORT_LEN);

        let stop = StopEvent::new()?;
        let stop_raw = stop.raw().0 as isize;
        let (tx, rx) = mpsc::channel();
        let mut readers = Vec::new();
        let mut open_one = |info: &HidInfo, n: usize| -> Result<Endpoint> {
            let h = std::sync::Arc::new(hid::open_rw(&info.path)?);
            let reader = h.clone();
            let tx = tx.clone();
            let len = info.input_len as usize;
            readers.push(std::thread::Builder::new().name(format!("hidpp-read-{n}")).spawn(
                move || {
                    let stop = HANDLE(stop_raw as *mut _);
                    let _ = hid::read_reports(n, &reader, len, stop, &tx);
                },
            )?);
            Ok(Endpoint { handle: h, out_len: info.output_len as usize })
        };
        let long = open_one(long_info, 0)?;
        let short = match short_info {
            Some(i) => Some(open_one(i, 1)?),
            None => None,
        };
        Ok(Channel {
            short,
            long,
            rx,
            stop,
            readers,
            queued: VecDeque::new(),
            label: label.to_string(),
            timeout: Duration::from_millis(1000),
        })
    }

    /// Reports that arrived and were not replies to us, oldest first.
    pub fn take_incoming(&mut self) -> Vec<Incoming> {
        self.pump(Duration::ZERO);
        self.queued.drain(..).collect()
    }

    /// Move everything already received into the queue, waiting up to `wait`
    /// for the first report.
    fn pump(&mut self, wait: Duration) {
        if let Ok(r) = self.rx.recv_timeout(wait) {
            self.queued.push_back(Incoming { at: r.at, data: r.data });
        }
        while let Ok(r) = self.rx.try_recv() {
            self.queued.push_back(Incoming { at: r.at, data: r.data });
        }
    }

    fn send(&self, report: &[u8]) -> Result<(), HidppError> {
        let ep = match report[0] {
            SHORT => self.short.as_ref().ok_or_else(|| {
                HidppError::Io(format!("{}: no short HID++ collection", self.label))
            })?,
            _ => &self.long,
        };
        hid::write_report(&ep.handle, report, ep.out_len)
            .map_err(|e| HidppError::Io(format!("{e:#}")))
    }

    /// Send `report` and wait for the reply matching `expect`.
    fn transact(&mut self, report: &[u8], expect: Expect) -> Result<Vec<u8>, HidppError> {
        // Anything that arrived before this request cannot be its reply.
        self.pump(Duration::ZERO);
        let first_new = self.queued.len();
        self.send(report)?;
        let deadline = Instant::now() + self.timeout;
        let mut checked = first_new;
        loop {
            // Look through what has arrived since the request went out.
            while checked < self.queued.len() {
                match classify(&self.queued[checked].data, expect) {
                    Match::Unrelated => checked += 1,
                    m => {
                        self.queued.remove(checked);
                        return match m {
                            Match::Reply(p) => Ok(p),
                            Match::Error10(c @ (0x04 | 0x09)) => Err(HidppError::Offline(c)),
                            Match::Error10(c) => Err(HidppError::Error10(c)),
                            Match::Error20(c) => Err(HidppError::Error20(c)),
                            Match::Unrelated => unreachable!(),
                        };
                    }
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(HidppError::Timeout);
            }
            match self.rx.recv_timeout(deadline - now) {
                Ok(r) => self.queued.push_back(Incoming { at: r.at, data: r.data }),
                Err(RecvTimeoutError::Timeout) => return Err(HidppError::Timeout),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(HidppError::Io(format!("{}: device gone", self.label)));
                }
            }
        }
    }

    /// HID++ 2.0 request: feature index `feat`, function `func`, up to 16
    /// parameter bytes. Retries a few times while the device says it is busy.
    pub fn request(
        &mut self,
        dev: u8,
        feat: u8,
        func: u8,
        params: &[u8],
    ) -> Result<Vec<u8>, HidppError> {
        assert!(params.len() <= 16, "HID++ long reports carry at most 16 parameter bytes");
        let addr = (func << 4) | SW_ID;
        let mut report = vec![LONG, dev, feat, addr];
        report.extend_from_slice(params);
        let expect = Expect { dev, sub: feat, addr };
        let mut backoff = [30u64, 60, 90].into_iter();
        loop {
            match self.transact(&report, expect) {
                Err(HidppError::Error20(0x08)) => match backoff.next() {
                    Some(ms) => std::thread::sleep(Duration::from_millis(ms)),
                    None => return Err(HidppError::Error20(0x08)),
                },
                other => return other,
            }
        }
    }

    /// HID++ 1.0 register read. `long` reads (sub-id 0x83) return 16 bytes,
    /// short ones (0x81) 3; both are requested with a short report.
    pub fn read_register(
        &mut self,
        dev: u8,
        register: u8,
        long: bool,
        params: [u8; 3],
    ) -> Result<Vec<u8>, HidppError> {
        let sub = if long { 0x83 } else { 0x81 };
        let report = [SHORT, dev, sub, register, params[0], params[1], params[2]];
        let saved = self.timeout;
        // Long register reads go through the receiver's slower path.
        if long {
            self.timeout = saved * 2;
        }
        let r = self.transact(&report, Expect { dev, sub, addr: register });
        self.timeout = saved;
        r
    }

    /// Protocol version via ROOT ping: `(major, minor)`. HID++ 1.0 devices
    /// answer with error 0x01 (invalid sub-id), reported here as (1, 0).
    pub fn ping(&mut self, dev: u8) -> Result<(u8, u8), HidppError> {
        const MARK: u8 = 0x5A;
        match self.request(dev, 0x00, 1, &[0, 0, MARK]) {
            Ok(p) if p.len() >= 3 && p[2] == MARK => Ok((p[0], p[1])),
            Ok(_) => Err(HidppError::Io("malformed ping reply".into())),
            Err(HidppError::Error10(0x01)) => Ok((1, 0)),
            Err(e) => Err(e),
        }
    }

    /// Index of a feature in the device's table: `Some((index, type, version))`,
    /// or `None` when the device does not have it.
    pub fn feature_index(&mut self, dev: u8, id: u16) -> Result<Option<(u8, u8, u8)>, HidppError> {
        let p = self.request(dev, 0x00, 0, &id.to_be_bytes())?;
        if p.len() < 3 || p[0] == 0 { Ok(None) } else { Ok(Some((p[0], p[1], p[2]))) }
    }

    /// The device's whole feature table: (index, id, type, version), ROOT included.
    pub fn feature_table(&mut self, dev: u8) -> Result<Vec<(u8, u16, u8, u8)>, HidppError> {
        let Some((fs, _, _)) = self.feature_index(dev, feature::FEATURE_SET)? else {
            return Err(HidppError::Io("no FEATURE_SET feature".into()));
        };
        let count = self.request(dev, fs, 0, &[])?.first().copied().unwrap_or(0);
        let mut out = vec![(0u8, feature::ROOT, 0u8, 0u8)];
        // getCount excludes ROOT; indices run 1..=count.
        for i in 1..=count {
            let p = self.request(dev, fs, 1, &[i])?;
            if p.len() >= 4 {
                out.push((i, u16::from_be_bytes([p[0], p[1]]), p[2], p[3]));
            }
        }
        Ok(out)
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.stop.set();
        for r in self.readers.drain(..) {
            let _ = r.join();
        }
    }
}

/// The HID++ collections of one physical USB device (same vendor, product
/// and interface), chosen by usage page, never by ids alone.
pub fn hidpp_collections(all: &[HidInfo], pid: u16) -> Vec<&HidInfo> {
    all.iter()
        .filter(|i| i.pid == pid)
        .filter(|i| crate::diag::caps::role(i).starts_with("HID++"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const E: Expect = Expect { dev: 0x01, sub: 0x0C, addr: 0x19 };

    #[test]
    fn our_reply_is_recognised_and_its_payload_returned() {
        let r = [0x11, 0x01, 0x0C, 0x19, 0x0F, 0xA0, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(classify(&r, E), Match::Reply(r[4..].to_vec()));
    }

    #[test]
    fn a_reply_with_another_software_id_is_not_ours() {
        // Same feature and function, but software id 0x0D (someone else).
        let r = [0x11, 0x01, 0x0C, 0x1D, 0x0F, 0xA0, 0x01];
        assert_eq!(classify(&r, E), Match::Unrelated);
    }

    #[test]
    fn an_unsolicited_event_is_not_a_reply() {
        let r = [0x11, 0x01, 0x0C, 0x00, 0x0F, 0xA0, 0x01];
        assert_eq!(classify(&r, E), Match::Unrelated);
    }

    #[test]
    fn the_receivers_error_for_a_sleeping_device_is_matched_to_the_request() {
        let r = [0x10, 0x01, 0x8F, 0x0C, 0x19, 0x09, 0x00];
        assert_eq!(classify(&r, E), Match::Error10(0x09));
        // ...but not to a different request.
        let other = Expect { dev: 0x01, sub: 0x0C, addr: 0x29 };
        assert_eq!(classify(&r, other), Match::Unrelated);
    }

    #[test]
    fn a_hidpp20_error_is_matched_to_the_request() {
        let r = [0x11, 0x01, 0xFF, 0x0C, 0x19, 0x08, 0x00];
        assert_eq!(classify(&r, E), Match::Error20(0x08));
    }

    #[test]
    fn a_reply_for_another_device_index_is_not_ours() {
        let r = [0x11, 0x02, 0x0C, 0x19, 0x0F, 0xA0, 0x01];
        assert_eq!(classify(&r, E), Match::Unrelated);
    }

    #[test]
    fn register_replies_match_on_sub_id_and_register() {
        let e = Expect { dev: RECEIVER, sub: 0x81, addr: 0x02 };
        let r = [0x10, 0xFF, 0x81, 0x02, 0x00, 0x01, 0x00];
        assert_eq!(classify(&r, e), Match::Reply(vec![0x00, 0x01, 0x00]));
    }
}
