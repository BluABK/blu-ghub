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
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
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

/// Pseudo-endpoints on the report queue: a message from the owner of the
/// channel, and a reader announcing that its device went away.
const CONTROL: usize = usize::MAX;
const GONE: usize = usize::MAX - 1;

/// Wakes a thread blocked in [`Channel::wait_event`] with a control code.
#[derive(Clone)]
pub struct Waker(Sender<Report>);

impl Waker {
    pub fn wake(&self, code: u8) {
        let _ = self.0.send(Report { endpoint: CONTROL, at: Instant::now(), data: vec![code] });
    }
}

/// What [`Channel::wait_event`] returns.
#[derive(Debug, Clone)]
pub enum Event {
    /// A report that was not a reply to one of our requests.
    Report(Incoming),
    /// A control code sent through a [`Waker`].
    Control(u8),
}

/// The "I am talking HID++ to these devices" lock, so two blu-ghub processes
/// (the tray app and a probe) never interleave requests. It is the named
/// mutex's *existence* that counts: whoever created it holds the lock until
/// its last handle closes. In the Global namespace, so it also covers a
/// second session on the same machine.
pub struct HidppLock(HANDLE);

unsafe impl Send for HidppLock {}

impl Drop for HidppLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Take the lock, or fail if another blu-ghub process has it.
pub fn acquire_lock() -> Result<HidppLock> {
    match try_lock()? {
        LockState::Acquired(l) => Ok(l),
        LockState::Busy(_) => {
            bail!("another blu-ghub process is talking to the devices (quit the tray app first)")
        }
    }
}

pub enum LockState {
    Acquired(HidppLock),
    /// Another process has it; this handle can wait for it to go.
    Busy(BusyLock),
}

pub fn try_lock() -> Result<LockState> {
    // Created owned: a process that only probes keeps it owned until it exits,
    // so a waiter can tell when it is gone.
    let h = unsafe { CreateMutexW(None, true, w!("Global\\BluGHub.hidpp")) }
        .context("creating the HID++ lock")?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Ok(LockState::Busy(BusyLock(h)));
    }
    Ok(LockState::Acquired(HidppLock(h)))
}

/// A handle to a lock someone else holds.
pub struct BusyLock(HANDLE);

unsafe impl Send for BusyLock {}

impl BusyLock {
    /// Block until the holder exits (its ownership is then abandoned to us),
    /// or until `stop` is signalled. On success this process holds the lock.
    pub fn wait(self, stop: HANDLE) -> Option<HidppLock> {
        use windows::Win32::Foundation::{WAIT_ABANDONED_0, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};
        let h = self.0;
        std::mem::forget(self);
        let r = unsafe { WaitForMultipleObjects(&[h, stop], false, INFINITE) };
        if r == WAIT_OBJECT_0 || r == WAIT_ABANDONED_0 {
            Some(HidppLock(h))
        } else {
            unsafe {
                let _ = CloseHandle(h);
            }
            None
        }
    }
}

impl Drop for BusyLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
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
    tx: Sender<Report>,
    stop: StopEvent,
    readers: Vec<JoinHandle<()>>,
    queued: VecDeque<Incoming>,
    controls: VecDeque<u8>,
    gone: bool,
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

        // Every handle first, then the channel, then the readers: if anything
        // fails after a reader is running, dropping the channel stops and
        // joins it (and only then closes the event it waits on).
        let long_h = std::sync::Arc::new(hid::open_rw(&long_info.path)?);
        let short_h = match short_info {
            Some(i) => Some(std::sync::Arc::new(hid::open_rw(&i.path)?)),
            None => None,
        };
        let (tx, rx) = mpsc::channel();
        let mut ch = Channel {
            short: short_h
                .as_ref()
                .zip(short_info)
                .map(|(h, i)| Endpoint { handle: h.clone(), out_len: i.output_len as usize }),
            long: Endpoint { handle: long_h.clone(), out_len: long_info.output_len as usize },
            rx,
            tx,
            stop: StopEvent::new()?,
            readers: Vec::new(),
            queued: VecDeque::new(),
            controls: VecDeque::new(),
            gone: false,
            label: label.to_string(),
            // A keyboard in power saving takes about a second to answer its first
            // request (measured: 989 ms); 1 s would sometimes mistake it for
            // asleep. Solaar allows 4 s for devices behind a receiver.
            timeout: Duration::from_secs(3),
        };
        ch.spawn_reader(0, long_h, long_info.input_len as usize)?;
        if let (Some(h), Some(i)) = (short_h, short_info) {
            ch.spawn_reader(1, h, i.input_len as usize)?;
        }
        Ok(ch)
    }

    /// Also read `info`, a collection of the same device that is not HID++,
    /// opened read-only: its reports arrive through [`Channel::wait_event`]
    /// with the rest. Their report ids never look like HID++ replies.
    pub fn listen(&mut self, info: &HidInfo) -> Result<()> {
        let handle = std::sync::Arc::new(hid::open_read(&info.path)?);
        let n = self.readers.len();
        self.spawn_reader(n, handle, info.input_len as usize)
    }

    /// The long collection's handle, for a last-resort write from a panic
    /// hook. Valid for as long as the channel lives.
    pub fn raw_long_handle(&self) -> isize {
        self.long.handle.0.0 as isize
    }

    fn spawn_reader(
        &mut self,
        n: usize,
        handle: std::sync::Arc<OwnedHandle>,
        len: usize,
    ) -> Result<()> {
        let stop_raw = self.stop.raw().0 as isize;
        let tx = self.tx.clone();
        let t = std::thread::Builder::new().name(format!("hidpp-read-{n}")).spawn(move || {
            let stop = HANDLE(stop_raw as *mut _);
            if let hid::ReaderEnd::Gone(why) = hid::read_reports(n, &handle, len, stop, &tx) {
                let report = Report { endpoint: GONE, at: Instant::now(), data: why.into_bytes() };
                let _ = tx.send(report);
            }
        })?;
        self.readers.push(t);
        Ok(())
    }

    /// A handle another thread can use to wake [`Channel::wait_event`].
    pub fn waker(&self) -> Waker {
        Waker(self.tx.clone())
    }

    /// Sort one report from the queue into its place.
    fn accept(&mut self, r: Report) {
        match r.endpoint {
            CONTROL => self.controls.extend(r.data.first()),
            GONE => self.gone = true,
            _ => self.queued.push_back(Incoming { at: r.at, data: r.data }),
        }
    }

    /// Block until something happens: a report that is not a reply to us, a
    /// control code, or `timeout` passing (`Ok(None)`; `None` waits for
    /// ever). An error means the device went away; the channel is then useless.
    pub fn wait_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>, HidppError> {
        loop {
            if let Some(c) = self.controls.pop_front() {
                return Ok(Some(Event::Control(c)));
            }
            if let Some(r) = self.queued.pop_front() {
                return Ok(Some(Event::Report(r)));
            }
            if self.gone {
                return Err(HidppError::Io(format!("{}: device gone", self.label)));
            }
            let got = match timeout {
                Some(t) => self.rx.recv_timeout(t),
                None => self.rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match got {
                Ok(r) => self.accept(r),
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => self.gone = true,
            }
        }
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
            self.accept(r);
        }
        while let Ok(r) = self.rx.try_recv() {
            self.accept(r);
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
        if self.gone {
            return Err(HidppError::Io(format!("{}: device gone", self.label)));
        }
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
                Ok(r) => {
                    self.accept(r);
                    if self.gone {
                        return Err(HidppError::Io(format!("{}: device gone", self.label)));
                    }
                }
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

    /// HID++ 2.0 request in a short report (up to 3 parameter bytes). Only a
    /// few commands are specified this way (on-board memory's "write end");
    /// the device must have a short collection.
    pub fn request_short(
        &mut self,
        dev: u8,
        feat: u8,
        func: u8,
        params: &[u8],
    ) -> Result<Vec<u8>, HidppError> {
        assert!(params.len() <= 3, "HID++ short reports carry at most 3 parameter bytes");
        let addr = (func << 4) | SW_ID;
        let mut report = vec![SHORT, dev, feat, addr];
        report.extend_from_slice(params);
        report.resize(SHORT_LEN, 0);
        self.transact(&report, Expect { dev, sub: feat, addr })
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

    /// HID++ 1.0 short register write (sub-id 0x80): three parameter bytes.
    pub fn write_register(
        &mut self,
        dev: u8,
        register: u8,
        params: [u8; 3],
    ) -> Result<(), HidppError> {
        let report = [SHORT, dev, 0x80, register, params[0], params[1], params[2]];
        self.transact(&report, Expect { dev, sub: 0x80, addr: register }).map(|_| ())
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

/// The HID++ collections of every device with product id `pid`, one group
/// per physical device: two receivers of the same model (a keyboard's and a
/// mouse's) must never be merged into one channel. Collections of the same
/// USB interface share their instance id up to its last `&`
/// (`7&1a2b3c4d&0&0000` and `...&0001`). Chosen by usage page, never by ids
/// alone.
pub fn hidpp_groups(all: &[HidInfo], pid: u16) -> Vec<Vec<&HidInfo>> {
    let mut groups: Vec<(String, Vec<&HidInfo>)> = Vec::new();
    let hidpp = all
        .iter()
        .filter(|i| i.pid == pid)
        .filter(|i| crate::diag::caps::role(i).starts_with("HID++"));
    for i in hidpp {
        let key = instance_key(&i.path);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, g)) => g.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    groups.into_iter().map(|(_, g)| g).collect()
}

/// The part of a HID collection's path shared by every collection of the same
/// USB interface.
fn instance_key(path: &str) -> String {
    let inst = path.split('#').nth(2).unwrap_or(path);
    inst.rsplit_once('&').map(|(k, _)| k).unwrap_or(inst).to_ascii_lowercase()
}

/// Another collection of the same USB interface as `path` (the headset's
/// telephony collection beside its HID++ one, say), by usage page.
pub fn sibling<'a>(all: &'a [HidInfo], path: &str, usage_page: u16) -> Option<&'a HidInfo> {
    let key = instance_key(path);
    all.iter().find(|i| i.usage_page == usage_page && instance_key(&i.path) == key)
}

/// The first device's HID++ collections, for tools that expect one.
pub fn hidpp_collections(all: &[HidInfo], pid: u16) -> Vec<&HidInfo> {
    hidpp_groups(all, pid).into_iter().next().unwrap_or_default()
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
    fn collections_of_one_interface_group_together_and_two_receivers_do_not() {
        let a1 = r"\\?\HID#VID_046D&PID_C547&MI_02&Col01#7&1a2b3c4d&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let a2 = r"\\?\HID#VID_046D&PID_C547&MI_02&Col02#7&1a2b3c4d&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let b1 = r"\\?\HID#VID_046D&PID_C547&MI_02&Col01#7&5e6f7a8b&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        assert_eq!(instance_key(a1), instance_key(a2));
        assert_ne!(instance_key(a1), instance_key(b1));
    }

    #[test]
    fn register_replies_match_on_sub_id_and_register() {
        let e = Expect { dev: RECEIVER, sub: 0x81, addr: 0x02 };
        let r = [0x10, 0xFF, 0x81, 0x02, 0x00, 0x01, 0x00];
        assert_eq!(classify(&r, e), Match::Reply(vec![0x00, 0x01, 0x00]));
    }
}
