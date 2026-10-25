//! `keys`: a key-timing logger that looks for the signatures of lost or late
//! key releases, per keyboard device.
//!
//! It uses Raw Input (`RIDEV_INPUTSINK`), which receives a copy of every
//! keystroke from every keyboard without sitting in the keystroke path: unlike
//! a hook it cannot delay or alter typing, even if it is slow or stuck.
//!
//! What it looks for:
//! - **Lost key-up.** A keyboard reports its full key state each time anything
//!   changes. If the report that said "key released" never arrives (radio
//!   interference, a driver dropping it), Windows keeps the key down and its
//!   auto-repeat produces "tttttt" until the *next* report — typically the next
//!   key press — carries the corrected state. So the signature is: the key that
//!   was auto-repeating is released within a few milliseconds of a different
//!   key's fresh press (the two come out of the same report).
//! - **Late key-up.** A text key (letter, digit, space...) that auto-repeated
//!   and was then released on its own. People rarely hold a letter down on
//!   purpose, so these are worth listing with their durations.
//! - **Chatter.** The same key pressed again within a few milliseconds of its
//!   release: a worn or bouncing switch, not the radio.
//!
//! Timing is only as good as the moment each event is processed, so the input
//! loop does nothing but classify: all console and file output happens on a
//! writer thread. If the loop still falls behind (the machine was busy), the
//! events concerned are marked late and get no timing verdicts.
//!
//! Privacy: keys that type text are never written by name. Lines about a
//! single key use an anonymous id that is random per run; the incident dump
//! numbers each *press* instead, so it shows the timing of a burst of typing
//! without revealing which presses were the same key.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_PACKET, VK_SCROLL};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_DEVNOTIFY, RIDEV_INPUTSINK, RIDI_DEVICENAME, RIM_TYPEKEYBOARD,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GIDC_ARRIVAL, GetMessageTime,
    GetMessageW, HWND_MESSAGE, MSG, PostQuitMessage, RI_KEY_BREAK, RI_KEY_E0, RI_KEY_E1,
    RegisterClassW, SetTimer, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_DESTROY, WM_INPUT,
    WM_INPUT_DEVICE_CHANGE, WM_TIMER, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

use super::{STOP, Tee, TeeThread, install_stop_handler, now_stamp};
use crate::keynames::ScanCode;

/// Two events closer than this count as one keyboard report.
const SAME_REPORT_US: u64 = 3_000;
/// A re-press of the same key this soon after its release is switch chatter;
/// even very fast typists need several times longer to repeat a letter.
const CHATTER_US: u64 = 25_000;
/// A key that is really held repeats at least this often: Windows' longest
/// typematic delay is 1 s and its slowest repeat period about 400 ms. A
/// "press" of a key we think is down, arriving later than this, means we
/// missed its release (it went to the lock screen, UAC, Ctrl+Alt+Del).
const MISSED_UP_US: u64 = 1_500_000;
/// Input processed this much later than it was queued is "late": its timing
/// no longer reflects when the key moved. Well above the ~16 ms resolution
/// of the message clock.
const LATE_MS: u32 = 50;
/// How much history the incident dump (Scroll Lock pressed twice) covers.
const RING_US: u64 = 15_000_000;
/// Scroll Lock twice within this long marks an incident.
///
/// Scroll Lock rather than Pause: in the classic console, Pause freezes the
/// window's output until the next key press, which would hold up the very
/// line reporting the incident. Pressing Scroll Lock twice also leaves its
/// light (and any program that reads it) as it was.
const MARK_DOUBLE_US: u64 = 600_000;
/// Ignore releases of keys we never saw pressed, and the "arrival" of every
/// keyboard already present, during the first moments of a run.
const WARMUP_US: u64 = 2_000_000;

#[derive(Default)]
struct KeyState {
    down: bool,
    down_at: u64,
    last_make_at: u64,
    repeats: u32,
    /// Time of the last release, and whether that event was late.
    last_up: Option<(u64, bool)>,
    /// Number of the current press, for the incident dump.
    press: u32,
}

/// A key that auto-repeated and has just been released, waiting a few
/// milliseconds to see whether another key's press arrives in the same report.
struct PendingRun {
    key: ScanCode,
    released_at: u64,
    late: bool,
    held_us: u64,
    repeats: u32,
}

#[derive(Default)]
struct DevState {
    pending: Vec<PendingRun>,
    /// The latest fresh press: (time, key, late).
    last_press: Option<(u64, ScanCode, bool)>,
    /// The key whose auto-repeat ran most recently. Windows repeats only the
    /// last key pressed, so this is the key a lost report would leave stuck.
    last_repeat_key: Option<ScanCode>,
}

#[derive(Default, Clone)]
struct DevStats {
    presses: u64,
    text_repeat_runs: u64,
    other_repeat_runs: u64,
    lost_up_text: u64,
    lost_up_other: u64,
    late_up_text: u64,
    chatter: u64,
    orphan_releases: u64,
    missed_releases: u64,
    longest_text_hold_us: u64,
    arrivals: u64,
    removals: u64,
}

impl DevStats {
    fn active(&self) -> bool {
        self.presses + self.arrivals + self.removals + self.orphan_releases > 0
    }
}

#[derive(Clone)]
struct RingEvent {
    t: u64,
    dev: usize,
    key: ScanCode,
    press: u32,
    what: &'static str,
}

/// Anonymous, collision-free ids for keys that must not be named.
///
/// Ids are drawn at random (from a per-run seed) the first time a key is
/// seen, so they say nothing about the key and cannot be correlated between
/// runs; a set guarantees no two keys share one.
struct AnonIds {
    rng: u64,
    ids: HashMap<ScanCode, u16>,
    used: HashSet<u16>,
}

impl AnonIds {
    fn new(seed: u64) -> Self {
        Self { rng: seed, ids: HashMap::new(), used: HashSet::new() }
    }

    /// splitmix64: tiny, and plenty for picking unpredictable ids.
    fn next(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn id(&mut self, k: ScanCode) -> u16 {
        if let Some(&id) = self.ids.get(&k) {
            return id;
        }
        let id = loop {
            let c = self.next() as u16;
            if self.used.insert(c) {
                break c;
            }
        };
        self.ids.insert(k, id);
        id
    }

    /// The only way a key is ever written in a per-key line: its name if it
    /// types nothing, otherwise an anonymous id. Never its code.
    fn label(&mut self, k: ScanCode) -> String {
        match k.name() {
            Some(n) => n.to_string(),
            None => format!("key#{:04x}", self.id(k)),
        }
    }
}

struct Logger {
    out: TeeThread,
    qpc_freq: i64,
    qpc_start: i64,
    started: Instant,
    deadline: Option<Instant>,
    next_summary: Instant,
    anon: AnonIds,
    devices: HashMap<isize, usize>,
    dev_names: Vec<String>,
    stats: Vec<DevStats>,
    dev_state: Vec<DevState>,
    keys: HashMap<(usize, ScanCode), KeyState>,
    ring: VecDeque<RingEvent>,
    next_press: u32,
    last_mark: Option<u64>,
    in_backlog: bool,
    late_events: u64,
    injected_events: u64,
    finished: bool,
}

thread_local! {
    static LOGGER: RefCell<Option<Logger>> = const { RefCell::new(None) };
}

pub fn run(minutes: Option<u64>) -> Result<()> {
    let mut tee = Tee::create("keys");
    tee.line(&format!("BluGHub-diag keys {} at {}", crate::version(), now_stamp()));
    tee.line("Watching every keyboard through Raw Input (no hook, cannot affect typing).");
    tee.line("Text keys appear only as anonymous ids. Press Scroll Lock twice quickly to mark an");
    tee.line("incident (dumps the last 15 s). Ctrl+C or closing this window stops and summarises.");
    tee.flush();

    let mut f = 0i64;
    let mut c = 0i64;
    unsafe {
        QueryPerformanceFrequency(&mut f)?;
        QueryPerformanceCounter(&mut c)?;
    }
    let started = Instant::now();
    // A per-run seed for the anonymous ids, so they cannot be correlated
    // between runs (or reversed by building a table once).
    let seed = std::collections::hash_map::RandomState::new().build_hasher().finish();
    LOGGER.with(|l| {
        *l.borrow_mut() = Some(Logger {
            out: TeeThread::spawn(tee),
            qpc_freq: f,
            qpc_start: c,
            started,
            deadline: minutes.map(|m| started + Duration::from_secs(m * 60)),
            next_summary: started + Duration::from_secs(3600),
            anon: AnonIds::new(seed),
            devices: HashMap::new(),
            dev_names: Vec::new(),
            stats: Vec::new(),
            dev_state: Vec::new(),
            keys: HashMap::new(),
            ring: VecDeque::new(),
            next_press: 1,
            last_mark: None,
            in_backlog: false,
            late_events: 0,
            injected_events: 0,
            finished: false,
        })
    });

    let hwnd = create_window()?;
    let rid = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x06,
        dwFlags: RIDEV_INPUTSINK | RIDEV_DEVNOTIFY,
        hwndTarget: hwnd,
    };
    unsafe { RegisterRawInputDevices(&[rid], std::mem::size_of::<RAWINPUTDEVICE>() as u32) }
        .context("registering for raw keyboard input")?;
    install_stop_handler(None, Some(hwnd));
    // Housekeeping tick: settles pending releases, writes the hourly summary,
    // honours --minutes. Once a second is plenty for all three.
    unsafe { SetTimer(Some(hwnd), 1, 1000, None) };

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
        unsafe { DispatchMessageW(&msg) };
    }
    // Normally already done in WM_DESTROY; this covers a loop that ended
    // some other way, so the summary is never lost.
    with_logger(|l| l.finish());
    Ok(())
}

fn create_window() -> Result<HWND> {
    let hinstance = unsafe { GetModuleHandleW(PCWSTR::null()) }?;
    let class = w!("BluGHubDiagKeys");
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance.into(),
        lpszClassName: class,
        ..Default::default()
    };
    unsafe { RegisterClassW(&wc) };
    // A message-only window: never shown, receives raw input regardless of
    // which application has the focus (RIDEV_INPUTSINK).
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("BluGHub key timing"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        )
    }
    .context("creating the message window")?;
    Ok(hwnd)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            // How long this input waited in the queue: the message clock is
            // stamped when the input was posted.
            let age_ms = unsafe { GetTickCount().wrapping_sub(GetMessageTime() as u32) };
            if let Some(raw) = read_raw(lparam) {
                with_logger(|l| l.on_input(&raw, age_ms));
            }
            // Raw input must still reach DefWindowProc so the system can clean up.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_INPUT_DEVICE_CHANGE => {
            let arrived = wparam.0 as u32 == GIDC_ARRIVAL;
            with_logger(|l| l.on_device_change(HANDLE(lparam.0 as *mut _), arrived));
            LRESULT(0)
        }
        WM_TIMER => {
            let stop = with_logger(|l| l.on_tick()).unwrap_or(false);
            if stop || STOP.load(std::sync::atomic::Ordering::SeqCst) {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            with_logger(|l| l.finish());
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn with_logger<R>(f: impl FnOnce(&mut Logger) -> R) -> Option<R> {
    LOGGER.with(|l| l.borrow_mut().as_mut().map(f))
}

/// The keyboard part of one WM_INPUT.
enum RawEvent {
    Key {
        device: HANDLE,
        key: ScanCode,
        vkey: u16,
        released: bool,
    },
    /// Synthesised input (SendInput, on-screen keyboard, remote desktop):
    /// counted but never classified or named. Its "scan code" is whatever the
    /// sending program chose — for typed Unicode it is the character itself.
    Injected,
}

fn read_raw(lparam: LPARAM) -> Option<RawEvent> {
    let h = HRAWINPUT(lparam.0 as *mut _);
    let header = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    // Only keyboards are registered, and a keyboard packet always fits a
    // RAWINPUT, so read straight into one (no separate size query).
    let mut raw = RAWINPUT::default();
    let mut size = std::mem::size_of::<RAWINPUT>() as u32;
    let got = unsafe {
        GetRawInputData(h, RID_INPUT, Some((&mut raw as *mut RAWINPUT).cast()), &mut size, header)
    };
    if got == u32::MAX || got == 0 || raw.header.dwType != RIM_TYPEKEYBOARD.0 {
        return None;
    }
    let kb = unsafe { raw.data.keyboard };
    // 0xFF is not a key: Windows uses it for the fake shift events it wraps
    // around some E0 keys, and for keyboard overrun.
    if kb.VKey == 0xFF {
        return None;
    }
    let device = raw.header.hDevice;
    if device.0.is_null() || kb.VKey == VK_PACKET.0 {
        return Some(RawEvent::Injected);
    }
    let flags = kb.Flags as u32;
    Some(RawEvent::Key {
        device,
        key: ScanCode { code: kb.MakeCode, e0: flags & RI_KEY_E0 != 0, e1: flags & RI_KEY_E1 != 0 },
        vkey: kb.VKey,
        released: flags & RI_KEY_BREAK != 0,
    })
}

/// A friendly name for a keyboard device.
fn device_label(handle: HANDLE) -> String {
    let mut chars = 0u32;
    unsafe { GetRawInputDeviceInfoW(Some(handle), RIDI_DEVICENAME, None, &mut chars) };
    let mut buf = vec![0u16; chars as usize + 1];
    let got = unsafe {
        GetRawInputDeviceInfoW(
            Some(handle),
            RIDI_DEVICENAME,
            Some(buf.as_mut_ptr().cast()),
            &mut chars,
        )
    };
    let path = if got == u32::MAX {
        String::new()
    } else {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    };
    label_for_path(&path)
}

pub fn label_for_path(path: &str) -> String {
    let p = path.to_ascii_uppercase();
    let what = if p.contains("VID_046D&PID_C547") || p.contains("VID_046D&PID_C541") {
        "G915 via LIGHTSPEED receiver"
    } else if p.contains("VID_046D&PID_C33E") {
        "G915 on its USB cable"
    } else if p.contains("PID_C232") || p.contains("LGHUB") {
        "G HUB virtual keyboard"
    } else if p.is_empty() {
        "unknown keyboard"
    } else {
        "other keyboard"
    };
    // VID/PID only: the full path carries instance ids that mean nothing here.
    let id = p
        .find("VID_")
        .map(|i| p[i..].split('#').next().unwrap_or("").to_string())
        .unwrap_or_default();
    if id.is_empty() { what.to_string() } else { format!("{what} [{id}]") }
}

impl Logger {
    fn now_us(&self) -> u64 {
        let mut c = 0i64;
        unsafe {
            let _ = QueryPerformanceCounter(&mut c);
        }
        (((c - self.qpc_start) as i128) * 1_000_000 / self.qpc_freq as i128) as u64
    }

    fn device_index(&mut self, h: HANDLE) -> usize {
        let key = h.0 as isize;
        if let Some(&i) = self.devices.get(&key) {
            return i;
        }
        let i = self.dev_names.len();
        let name = device_label(h);
        self.out.line(format!("{} device {i}: {name}", now_stamp()));
        self.dev_names.push(name);
        self.stats.push(DevStats::default());
        self.dev_state.push(DevState::default());
        self.devices.insert(key, i);
        i
    }

    fn note_backlog(&mut self, age_ms: u32) -> bool {
        let late = age_ms > LATE_MS;
        if late {
            self.late_events += 1;
            if !self.in_backlog {
                self.in_backlog = true;
                self.out.line(format!(
                    "{} BACKLOG: input is being processed {age_ms} ms late; no timing verdicts \
                     until it catches up",
                    now_stamp()
                ));
            }
        } else if self.in_backlog {
            self.in_backlog = false;
            self.out.line(format!("{} backlog cleared", now_stamp()));
        }
        late
    }

    fn on_input(&mut self, ev: &RawEvent, age_ms: u32) {
        let RawEvent::Key { device, key, vkey, released } = *ev else {
            self.injected_events += 1;
            return;
        };
        let t = self.now_us();
        let late = self.note_backlog(age_ms);
        let dev = self.device_index(device);
        self.on_key(dev, key, vkey, released, t, late);
    }

    /// Classify one key event at time `t` (µs since start) on device `dev`.
    fn on_key(&mut self, dev: usize, key: ScanCode, vkey: u16, released: bool, t: u64, late: bool) {
        // A make for a key we think is down is a repeat, unless it came so
        // long after the previous one that we must have missed a release.
        let (was_down, last_make_at) =
            self.keys.get(&(dev, key)).map(|s| (s.down, s.last_make_at)).unwrap_or((false, 0));
        let is_repeat = !released && was_down && t.saturating_sub(last_make_at) <= MISSED_UP_US;
        let is_press = !released && !is_repeat;

        self.settle_pending(dev, t, Some((key, is_press, late)));

        let what = if released {
            self.on_release(dev, key, t, late)
        } else if is_repeat {
            self.on_repeat(dev, key, t)
        } else {
            self.on_press(dev, key, vkey, t, late, was_down)
        };

        let press = self.keys.get(&(dev, key)).map(|s| s.press).unwrap_or(0);
        self.ring.push_back(RingEvent { t, dev, key, press, what });
        while self.ring.front().is_some_and(|e| t.saturating_sub(e.t) > RING_US) {
            self.ring.pop_front();
        }
    }

    fn on_release(&mut self, dev: usize, key: ScanCode, t: u64, late: bool) -> &'static str {
        let state = self.keys.entry((dev, key)).or_default();
        if !state.down {
            state.last_up = Some((t, late));
            if t > WARMUP_US {
                self.stats[dev].orphan_releases += 1;
                let label = self.anon.label(key);
                self.out.line(format!(
                    "{} ORPHAN RELEASE  dev {dev}: {label} released without a press",
                    now_stamp()
                ));
            }
            return "up (never seen down)";
        }
        let held = t.saturating_sub(state.down_at);
        let repeats = state.repeats;
        state.down = false;
        state.repeats = 0;
        state.last_up = Some((t, late));
        if repeats == 0 {
            return "up";
        }
        // Case 1: the other key's fresh press came first in the same report.
        let ds = &self.dev_state[dev];
        let lost = ds.last_press.and_then(|(pt, pk, plate)| {
            (pk != key
                && t.saturating_sub(pt) <= SAME_REPORT_US
                && !late
                && !plate
                && ds.last_repeat_key == Some(key))
            .then(|| t - pt)
        });
        match lost {
            Some(dt) => self.lost_key_up(dev, key, held, repeats, dt),
            // Case 2: it may still follow; decide within SAME_REPORT_US.
            None => self.dev_state[dev].pending.push(PendingRun {
                key,
                released_at: t,
                late,
                held_us: held,
                repeats,
            }),
        }
        "up"
    }

    fn on_repeat(&mut self, dev: usize, key: ScanCode, t: u64) -> &'static str {
        let state = self.keys.entry((dev, key)).or_default();
        state.repeats += 1;
        state.last_make_at = t;
        let first = state.repeats == 1;
        self.dev_state[dev].last_repeat_key = Some(key);
        if first {
            if key.is_text() {
                self.stats[dev].text_repeat_runs += 1;
            } else {
                self.stats[dev].other_repeat_runs += 1;
            }
        }
        "repeat"
    }

    fn on_press(
        &mut self,
        dev: usize,
        key: ScanCode,
        vkey: u16,
        t: u64,
        late: bool,
        was_down: bool,
    ) -> &'static str {
        let press = self.next_press;
        self.next_press = self.next_press.wrapping_add(1);
        let state = self.keys.entry((dev, key)).or_default();
        let missed_gap = was_down.then(|| t.saturating_sub(state.last_make_at));
        let chatter_gap = state
            .last_up
            .filter(|&(u, ulate)| !late && !ulate && t.saturating_sub(u) < CHATTER_US)
            .map(|(u, _)| t - u);
        state.down = true;
        state.down_at = t;
        state.last_make_at = t;
        state.repeats = 0;
        state.press = press;
        self.dev_state[dev].last_press = Some((t, key, late));
        self.stats[dev].presses += 1;

        if let Some(gap) = missed_gap {
            self.stats[dev].missed_releases += 1;
            let label = self.anon.label(key);
            self.out.file_only(format!(
                "{} missed release  dev {dev}: {label} pressed again {:.1} s after it last went \
                 down, with no release in between (it went to the lock screen, UAC or another \
                 desktop); counted as a new press",
                now_stamp(),
                gap as f64 / 1e6
            ));
        }
        if let Some(gap) = chatter_gap {
            self.stats[dev].chatter += 1;
            let label = self.anon.label(key);
            self.out.line(format!(
                "{} CHATTER         dev {dev}: {label} pressed again {:.1} ms after its release",
                now_stamp(),
                gap as f64 / 1000.0
            ));
        }
        if vkey == VK_SCROLL.0 {
            if self.last_mark.is_some_and(|p| t.saturating_sub(p) < MARK_DOUBLE_US) {
                self.last_mark = None;
                self.dump_incident(t);
            } else {
                self.last_mark = Some(t);
            }
        }
        "down"
    }

    /// Decide pending repeat-run releases on `dev` in the light of a new
    /// event `(key, is_fresh_press, late)`, or of time passing (`None`).
    ///
    /// A fresh press of another key within one report's span means the run
    /// that was auto-repeating had lost its release. Breaks and repeats inside
    /// that span leave runs pending (a report can release several keys before
    /// pressing the new one). Past the span, a run was released on its own.
    fn settle_pending(&mut self, dev: usize, t: u64, event: Option<(ScanCode, bool, bool)>) {
        if self.dev_state[dev].pending.is_empty() {
            return;
        }
        let repeating = self.dev_state[dev].last_repeat_key;
        let mut lost = Vec::new();
        let mut done = Vec::new();
        let mut keep = Vec::new();
        for p in std::mem::take(&mut self.dev_state[dev].pending) {
            let dt = t.saturating_sub(p.released_at);
            match event {
                Some((k, true, late)) if k != p.key && dt <= SAME_REPORT_US => {
                    if !late && !p.late && repeating == Some(p.key) {
                        lost.push((p, dt));
                    } else {
                        done.push(p);
                    }
                }
                Some((k, true, _)) if k == p.key => done.push(p),
                _ if dt > SAME_REPORT_US => done.push(p),
                _ => keep.push(p),
            }
        }
        self.dev_state[dev].pending = keep;
        for (p, dt) in lost {
            self.lost_key_up(dev, p.key, p.held_us, p.repeats, dt);
        }
        for p in done {
            self.released_after_repeat(dev, p);
        }
    }

    fn lost_key_up(&mut self, dev: usize, key: ScanCode, held: u64, repeats: u32, dt: u64) {
        let label = self.anon.label(key);
        let (tag, note) = if key.is_text() {
            self.stats[dev].lost_up_text += 1;
            ("LOST KEY-UP?   ", "")
        } else {
            self.stats[dev].lost_up_other += 1;
            ("lost key-up?   ", " (a key people often hold; weaker evidence)")
        };
        self.out.line(format!(
            "{} {tag} dev {dev}: {label} auto-repeated {repeats}x over {:.2} s; its release came \
             {:.1} ms from the next key's press{note}",
            now_stamp(),
            held as f64 / 1e6,
            dt as f64 / 1000.0
        ));
    }

    fn released_after_repeat(&mut self, dev: usize, p: PendingRun) {
        if !p.key.is_text() {
            return;
        }
        self.stats[dev].late_up_text += 1;
        let s = &mut self.stats[dev];
        s.longest_text_hold_us = s.longest_text_hold_us.max(p.held_us);
        let label = self.anon.label(p.key);
        self.out.line(format!(
            "{} TEXT KEY HELD   dev {dev}: {label} held {:.2} s ({}x auto-repeat), released on its own",
            now_stamp(),
            p.held_us as f64 / 1e6,
            p.repeats
        ));
    }

    fn on_device_change(&mut self, h: HANDLE, arrived: bool) {
        // RIDEV_DEVNOTIFY announces every keyboard already present as an
        // "arrival" right after registering. Those are the starting lineup,
        // not events; the first lookup logs them.
        if arrived && self.now_us() < WARMUP_US {
            self.device_index(h);
            return;
        }
        let dev = self.device_index(h);
        if arrived {
            self.stats[dev].arrivals += 1;
        } else {
            self.stats[dev].removals += 1;
            // Its keys can no longer be released; forget them. Windows may
            // reuse the handle for another device, so forget that too.
            self.keys.retain(|(d, _), _| *d != dev);
            self.dev_state[dev] = DevState::default();
            self.devices.remove(&(h.0 as isize));
        }
        self.out.line(format!(
            "{} keyboard {} dev {dev}: {}",
            now_stamp(),
            if arrived { "ARRIVED " } else { "REMOVED " },
            self.dev_names[dev]
        ));
    }

    /// Returns true when the run should end (`--minutes` reached).
    fn on_tick(&mut self) -> bool {
        let t = self.now_us();
        for dev in 0..self.dev_state.len() {
            self.settle_pending(dev, t, None);
        }
        let now = Instant::now();
        if now >= self.next_summary {
            self.summary();
            // From now, not from the old target: after a sleep or a long
            // stall this writes one summary, not one per missed hour.
            self.next_summary = now + Duration::from_secs(3600);
        }
        self.deadline.is_some_and(|d| now >= d)
    }

    /// Write what led up to a marked incident (Scroll Lock pressed twice).
    fn dump_incident(&mut self, t: u64) {
        self.out.line(format!(
            "{} INCIDENT marked (Scroll Lock x2). Auto-repeats in the last {} s and what surrounded them:",
            now_stamp(),
            RING_US / 1_000_000
        ));
        let events: Vec<RingEvent> = self.ring.iter().cloned().collect();
        for line in incident_lines(&events, t) {
            self.out.line(line);
        }
    }

    fn summary(&mut self) {
        let mins = self.started.elapsed().as_secs_f64() / 60.0;
        self.out.line(format!("--- summary after {mins:.1} min ({}) ---", now_stamp()));
        let mut quiet = 0;
        for (i, s) in self.stats.clone().iter().enumerate() {
            if !s.active() {
                quiet += 1;
                continue;
            }
            self.out.line(format!("  dev {i}: {}", self.dev_names[i]));
            self.out.line(format!(
                "    presses {}  | lost key-up suspects: text {} other {}  | text keys held \
                 past repeat {} (longest {:.2} s)",
                s.presses,
                s.lost_up_text,
                s.lost_up_other,
                s.late_up_text,
                s.longest_text_hold_us as f64 / 1e6
            ));
            self.out.line(format!(
                "    auto-repeat runs: text {} other {}  | chatter {}  | orphan releases {}  | \
                 missed releases {}  | device arrivals {} removals {}",
                s.text_repeat_runs,
                s.other_repeat_runs,
                s.chatter,
                s.orphan_releases,
                s.missed_releases,
                s.arrivals,
                s.removals
            ));
        }
        if quiet > 0 {
            self.out.line(format!("  ({quiet} other keyboard device(s) with no activity)"));
        }
        self.out.line(format!(
            "  injected (synthesised) key events: {}  | events processed late: {}",
            self.injected_events, self.late_events
        ));
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let t = self.now_us();
        for dev in 0..self.dev_state.len() {
            self.settle_pending(dev, t + SAME_REPORT_US + 1, None);
        }
        self.summary();
        self.out.line(format!("Written to {}", self.out.path.display()));
        self.out.close();
    }
}

/// Events within this distance of an auto-repeating key's press or release
/// are shown in an incident dump; everything else is only counted.
const CONTEXT_US: u64 = 50_000;

/// The incident dump, reduced to what the diagnosis needs.
///
/// A "tttt" incident is an auto-repeat, so only presses that auto-repeated
/// are shown — when they went down, how long they repeated, when they came
/// up — plus any other key event within 50 ms of those moments (the press
/// that may have ended the run). All other typing in the window is reduced to
/// a count. Text keys appear as press numbers ("p17"), never as keys, so even
/// the shown events say nothing about what was typed; and without the rest of
/// the window there is no rhythm or structure (word lengths, Enter after a
/// password) to read either.
fn incident_lines(events: &[RingEvent], now: u64) -> Vec<String> {
    let ago = |t: u64| now.saturating_sub(t) as f64 / 1e6;
    let label = |e: &RingEvent| match e.key.name() {
        Some(n) => n.to_string(),
        None => format!("p{}", e.press),
    };

    // Presses that auto-repeated, by (device, press number), with their
    // first and last repeat and how many there were.
    let mut runs: HashMap<(usize, u32), (u64, u64, u32)> = HashMap::new();
    for e in events.iter().filter(|e| e.what == "repeat") {
        let r = runs.entry((e.dev, e.press)).or_insert((e.t, e.t, 0));
        r.1 = e.t;
        r.2 += 1;
    }
    if runs.is_empty() {
        return vec![format!(
            "    no key auto-repeated in the window ({} key events not shown)",
            events.len()
        )];
    }

    // The moments that matter: each repeating press's down, last repeat and up.
    let mut anchors: Vec<u64> = runs.values().map(|r| r.1).collect();
    anchors.extend(
        events
            .iter()
            .filter(|e| e.what != "repeat" && runs.contains_key(&(e.dev, e.press)))
            .map(|e| e.t),
    );
    let near = |t: u64| anchors.iter().any(|&a| t.abs_diff(a) <= CONTEXT_US);

    let mut out = Vec::new();
    let mut hidden = 0usize;
    for e in events {
        let id = (e.dev, e.press);
        match runs.get(&id) {
            Some(&(first, last, n)) if e.what == "repeat" => {
                // One line per run, written at its first repeat.
                if e.t == first {
                    out.push(format!(
                        "    -{:>7.3} s  dev {}  {:<12} auto-repeat x{n} until -{:.3} s",
                        ago(first),
                        e.dev,
                        label(e),
                        ago(last)
                    ));
                }
            }
            Some(_) => out.push(format!(
                "    -{:>7.3} s  dev {}  {:<12} {}",
                ago(e.t),
                e.dev,
                label(e),
                e.what
            )),
            None if near(e.t) => out.push(format!(
                "    -{:>7.3} s  dev {}  {:<12} {}",
                ago(e.t),
                e.dev,
                label(e),
                e.what
            )),
            None => hidden += 1,
        }
    }
    out.push(format!(
        "    ({hidden} other key events in the window not shown: not near any auto-repeat)"
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_are_named_by_what_they_are_not_their_instance_path() {
        let rx = r"\\?\HID#VID_046D&PID_C547&MI_01&Col01#7&12ab34cd&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
        assert_eq!(
            label_for_path(rx),
            "G915 via LIGHTSPEED receiver [VID_046D&PID_C547&MI_01&COL01]"
        );
        let ghub =
            r"\\?\HID#VID_046D&PID_C232#1&5e6f7a8b&0&0000#{884b96c3-56ef-11d1-bc8c-00a0c91405dd}";
        assert!(label_for_path(ghub).starts_with("G HUB virtual keyboard"));
        assert_eq!(label_for_path(""), "unknown keyboard");
    }

    /// The privacy promise, checked exhaustively: whatever key Raw Input
    /// reports, its label is either a non-text key's name or an anonymous id.
    #[test]
    fn no_key_is_ever_labelled_by_its_code() {
        let mut anon = AnonIds::new(0x1234_5678);
        for code in 0..=0x1FFu16 {
            for (e0, e1) in [(false, false), (true, false), (false, true)] {
                let k = ScanCode { code, e0, e1 };
                let label = anon.label(k);
                match k.name() {
                    Some(n) => assert_eq!(label, n),
                    None => assert!(label.starts_with("key#") && label.len() == 8, "{label}"),
                }
                assert!(!(k.is_text() && k.name().is_some()), "text key with a name: {k:?}");
            }
        }
    }

    // --- classification, replaying event sequences with explicit times ---

    const A: ScanCode = ScanCode { code: 0x1E, e0: false, e1: false }; // a text key
    const B: ScanCode = ScanCode { code: 0x30, e0: false, e1: false }; // another text key
    const W: ScanCode = ScanCode { code: 0x11, e0: false, e1: false };
    const D: ScanCode = ScanCode { code: 0x20, e0: false, e1: false };
    const MS: u64 = 1_000;

    fn logger() -> Logger {
        let mut l = Logger {
            out: TeeThread::spawn(Tee::sink()),
            qpc_freq: 1,
            qpc_start: 0,
            started: Instant::now(),
            deadline: None,
            next_summary: Instant::now() + Duration::from_secs(3600),
            anon: AnonIds::new(7),
            devices: HashMap::new(),
            dev_names: vec!["test keyboard".into()],
            stats: vec![DevStats::default()],
            dev_state: vec![DevState::default()],
            keys: HashMap::new(),
            ring: VecDeque::new(),
            next_press: 1,
            last_mark: None,
            in_backlog: false,
            late_events: 0,
            injected_events: 0,
            finished: false,
        };
        // Start well past the warm-up window.
        l.settle_pending(0, 10_000 * MS, None);
        l
    }

    const T0: u64 = 10_000 * MS;

    fn down(l: &mut Logger, k: ScanCode, t: u64) {
        l.on_key(0, k, 0, false, T0 + t, false);
    }
    fn up(l: &mut Logger, k: ScanCode, t: u64) {
        l.on_key(0, k, 0, true, T0 + t, false);
    }
    /// Hold `k` from `t` with Windows' default typematic: first repeat after
    /// 500 ms, then every 33 ms, until `until`.
    fn hold_repeating(l: &mut Logger, k: ScanCode, t: u64, until: u64) {
        down(l, k, t);
        let mut r = t + 500 * MS;
        while r < until {
            down(l, k, r);
            r += 33 * MS;
        }
    }
    fn tick(l: &mut Logger, t: u64) {
        l.settle_pending(0, T0 + t, None);
    }

    #[test]
    fn a_stuck_key_released_by_the_next_press_is_a_lost_key_up_in_either_order() {
        // Release first, then the new press, 0.2 ms apart (one report).
        let mut l = logger();
        hold_repeating(&mut l, A, 0, 1_200 * MS);
        up(&mut l, A, 1_200 * MS);
        down(&mut l, B, 1_200 * MS + 200);
        assert_eq!(l.stats[0].lost_up_text, 1);
        assert_eq!(l.stats[0].late_up_text, 0);

        // New press first, then the release.
        let mut l = logger();
        hold_repeating(&mut l, A, 0, 1_200 * MS);
        down(&mut l, B, 1_200 * MS);
        up(&mut l, A, 1_200 * MS + 200);
        assert_eq!(l.stats[0].lost_up_text, 1);
    }

    #[test]
    fn rolling_from_one_held_key_to_another_is_not_a_lost_key_up() {
        // Hold W until it repeats, press D (which takes over the repeat),
        // then let go of W 1 ms before one of D's repeats arrives. A repeat
        // is not a press, so this must not read as "W's release came with
        // the next key's press".
        let mut l = logger();
        hold_repeating(&mut l, W, 0, 900 * MS);
        down(&mut l, D, 900 * MS);
        let d_repeat = 900 * MS + 500 * MS;
        down(&mut l, D, d_repeat);
        up(&mut l, W, d_repeat + 32 * MS);
        down(&mut l, D, d_repeat + 33 * MS);
        tick(&mut l, d_repeat + 100 * MS);
        assert_eq!(l.stats[0].lost_up_text, 0);
        // W really was held past its auto-repeat; that is reported as such.
        assert_eq!(l.stats[0].late_up_text, 1);
    }

    #[test]
    fn a_held_key_released_on_its_own_is_a_late_up_not_a_lost_one() {
        let mut l = logger();
        hold_repeating(&mut l, A, 0, 800 * MS);
        up(&mut l, A, 800 * MS);
        down(&mut l, B, 950 * MS);
        assert_eq!(l.stats[0].lost_up_text, 0);
        assert_eq!(l.stats[0].late_up_text, 1);
    }

    #[test]
    fn a_release_lost_to_the_lock_screen_is_not_mistaken_for_auto_repeat() {
        // Pressed, then the release went to another desktop; minutes later
        // the key is pressed again.
        let mut l = logger();
        down(&mut l, A, 0);
        down(&mut l, A, 120_000 * MS);
        up(&mut l, A, 120_100 * MS);
        down(&mut l, B, 120_100 * MS + 100);
        assert_eq!(l.stats[0].missed_releases, 1);
        assert_eq!(l.stats[0].presses, 3);
        assert_eq!(l.stats[0].text_repeat_runs, 0);
        assert_eq!(l.stats[0].lost_up_text, 0);
    }

    #[test]
    fn a_bounce_within_a_few_ms_is_chatter_but_a_fast_double_letter_is_not() {
        let mut l = logger();
        down(&mut l, A, 0);
        up(&mut l, A, 60 * MS);
        down(&mut l, A, 65 * MS);
        assert_eq!(l.stats[0].chatter, 1);

        let mut l = logger();
        down(&mut l, A, 0);
        up(&mut l, A, 60 * MS);
        down(&mut l, A, 140 * MS);
        assert_eq!(l.stats[0].chatter, 0);
    }

    #[test]
    fn late_events_get_no_timing_verdicts() {
        let mut l = logger();
        hold_repeating(&mut l, A, 0, 1_200 * MS);
        l.on_key(0, A, 0, true, T0 + 1_200 * MS, true);
        l.on_key(0, B, 0, false, T0 + 1_200 * MS + 10, true);
        assert_eq!(l.stats[0].lost_up_text, 0);
    }

    #[test]
    fn an_incident_dump_shows_the_stuck_key_and_its_neighbours_but_not_the_typing() {
        let mut l = logger();
        // Ordinary typing well before the incident: must not appear.
        for (i, k) in [A, B, A, B, A].into_iter().enumerate() {
            let t = (i as u64) * 150 * MS;
            down(&mut l, k, t);
            up(&mut l, k, t + 60 * MS);
        }
        // The incident: A sticks and repeats until B's press releases it.
        hold_repeating(&mut l, A, 3_000 * MS, 4_000 * MS);
        down(&mut l, B, 4_000 * MS);
        up(&mut l, A, 4_000 * MS + 200);
        up(&mut l, B, 4_080 * MS);

        let events: Vec<RingEvent> = l.ring.iter().cloned().collect();
        let lines = incident_lines(&events, T0 + 5_000 * MS);
        let text = lines.join(
            "
",
        );
        assert!(text.contains("auto-repeat x"), "{text}");
        // The stuck press: down, the repeat run, up; plus B's press 0.2 ms
        // away. B's release 80 ms later is outside the context window.
        assert_eq!(lines.len(), 5, "{text}");
        assert!(text.contains("(11 other key events in the window not shown"), "{text}");
        // No line names a text key.
        assert!(!text.contains("key#"), "{text}");
    }

    #[test]
    fn anonymous_ids_never_collide_and_are_stable_within_a_run() {
        let mut anon = AnonIds::new(42);
        let keys: Vec<ScanCode> =
            (0..0x60u16).map(|code| ScanCode { code, e0: false, e1: false }).collect();
        let ids: HashSet<u16> = keys.iter().map(|&k| anon.id(k)).collect();
        assert_eq!(ids.len(), keys.len());
        assert_eq!(anon.id(keys[5]), anon.id(keys[5]));
    }
}
