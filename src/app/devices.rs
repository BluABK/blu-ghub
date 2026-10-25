//! One worker thread per device connection, each owning its HID++ channel.
//!
//! A worker spends its life blocked in [`Channel::wait_event`]: it wakes when
//! the device sends something (a battery change, the keyboard waking or going
//! to sleep, the headset switching off or on), when the tray asks for a
//! refresh, or — only while the device is awake — when the fallback reading is
//! due. While a device is asleep or off the wait has no timeout at all:
//! nothing is sent to it until it announces itself.
//!
//! What a worker learns goes into the shared [`Status`]; then it posts
//! `WM_APP_STATE` so the tray redraws.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use anyhow::{Context, anyhow, bail};
use windows::core::GUID;

use crate::audio::{Com, Mic};
use crate::battery::{self, Battery, HeadsetPower};
use crate::hid::HidInfo;
use crate::hidpp::{self, Channel, Event, HidppError, RECEIVER, Waker, feature};
use crate::input::ButtonAction;

pub const WM_APP_STATE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

/// Control codes for a worker.
pub const REFRESH: u8 = 1;
pub const QUIT: u8 = 2;

/// A device as the user sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Keyboard,
    Headset,
}

impl Device {
    pub fn name(self) -> &'static str {
        match self {
            Device::Keyboard => "G915",
            Device::Headset => "G935",
        }
    }
}

/// A connection a worker serves. The G915 has two: through its LIGHTSPEED
/// receiver, and on its USB cable (while charging).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Link {
    KeyboardReceiver,
    KeyboardCable,
    Headset,
}

impl Link {
    pub const ALL: [Link; 3] = [Link::KeyboardCable, Link::KeyboardReceiver, Link::Headset];

    fn label(self) -> &'static str {
        match self {
            Link::KeyboardReceiver => "G915",
            Link::KeyboardCable => "G915 (cable)",
            Link::Headset => "G935",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// No receiver, cable or dongle.
    Absent,
    /// The receiver or dongle is there; the device is asleep, off or out of range.
    Sleeping,
    Connected,
}

/// What is known about one connection.
#[derive(Debug, Clone)]
pub struct LinkStatus {
    pub state: State,
    /// The last reading, kept while the device sleeps.
    pub battery: Option<Battery>,
}

impl Default for LinkStatus {
    fn default() -> Self {
        Self { state: State::Absent, battery: None }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub keyboard_receiver: LinkStatus,
    pub keyboard_cable: LinkStatus,
    pub headset: LinkStatus,
}

impl Status {
    fn link_mut(&mut self, link: Link) -> &mut LinkStatus {
        match link {
            Link::KeyboardReceiver => &mut self.keyboard_receiver,
            Link::KeyboardCable => &mut self.keyboard_cable,
            Link::Headset => &mut self.headset,
        }
    }

    /// The device's status: for the keyboard, the cable wins while it is
    /// plugged in (the radio link is down meanwhile).
    pub fn device(&self, device: Device) -> &LinkStatus {
        match device {
            Device::Keyboard if self.keyboard_cable.state != State::Absent => &self.keyboard_cable,
            Device::Keyboard => &self.keyboard_receiver,
            Device::Headset => &self.headset,
        }
    }
}

pub type Shared = Arc<Mutex<Status>>;

/// A running worker.
pub struct Worker {
    pub link: Link,
    waker: Waker,
    finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }
    pub fn refresh(&self) {
        self.waker.wake(REFRESH);
    }
    /// Ask the worker to stop and wait for it (it closes its channel). At
    /// most a few seconds: a request in flight finishes or times out first.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.waker.wake(QUIT);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A worker dropped without [`Worker::stop`] (the app torn down some other
/// way) still stops cleanly: the headset's worker hands its buttons back.
impl Drop for Worker {
    fn drop(&mut self) {
        self.halt();
    }
}

/// Context a worker needs to report back.
#[derive(Clone)]
pub struct Reporter {
    pub shared: Shared,
    pub hwnd: isize,
}

impl Reporter {
    fn update(&self, link: Link, f: impl FnOnce(&mut LinkStatus)) {
        {
            let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
            f(s.link_mut(link));
        }
        unsafe {
            let _ =
                PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_APP_STATE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Which receiver slot holds the G915.
enum Slot {
    Found(u8),
    /// The pairing table was read and has no G915.
    NotHere,
    /// The pairing table could not be read.
    Unknown,
}

/// The receiver's slot for the G915 (wireless product id 0x407C), from its
/// pairing register. A reply only counts if it echoes the slot asked about:
/// another program may be reading the same register.
fn keyboard_slot(ch: &mut Channel) -> Slot {
    let mut any_read = false;
    for slot in 0..6u8 {
        for _attempt in 0..3 {
            match ch.read_register(RECEIVER, 0xB5, true, [0x20 + slot, 0, 0]) {
                Ok(p) if p.first() == Some(&(0x20 + slot)) => {
                    any_read = true;
                    if p.len() >= 5 && p[3] == 0x40 && p[4] == 0x7C {
                        return Slot::Found(slot + 1);
                    }
                    break;
                }
                // Someone else's reply to the same register: ask again.
                Ok(_) => continue,
                // An empty slot answers with an error; so does nothing else.
                Err(_) => {
                    any_read = true;
                    break;
                }
            }
        }
    }
    if any_read { Slot::NotHere } else { Slot::Unknown }
}

/// Make sure the receiver tells us when a device connects or disconnects.
/// The setting (register 0x00, bit 0x000100) lives in the receiver's RAM and
/// is lost when it is unplugged; G HUB used to turn it on. Only the receiver
/// is involved, never the keyboard.
fn enable_link_notifications(ch: &mut Channel) {
    match ch.read_register(RECEIVER, 0x00, false, [0; 3]) {
        Ok(p) if p.len() >= 3 && p[1] & 0x01 == 0 => {
            match ch.write_register(RECEIVER, 0x00, [p[0], p[1] | 0x01, p[2]]) {
                Ok(()) => info!("receiver: turned on connection notifications"),
                Err(e) => warn!("receiver: could not turn on connection notifications: {e}"),
            }
        }
        Ok(_) => {}
        Err(e) => warn!("receiver: could not read its notification flags: {e}"),
    }
}

/// Start a worker for `link` if its hardware is among `all`.
pub fn start(
    link: Link,
    all: &[HidInfo],
    rep: Reporter,
    poll: Duration,
    buttons: [ButtonAction; 3],
) -> Option<Worker> {
    let (pid, label) = match link {
        Link::KeyboardReceiver => (0xC547, "G915 receiver"),
        Link::KeyboardCable => (0xC33E, "G915 cable"),
        Link::Headset => (0x0A87, "G935"),
    };
    let groups = hidpp::hidpp_groups(all, pid);
    let only_one = groups.len() == 1;
    let mut chosen = None;
    for cols in groups {
        let path = cols[0].path.clone();
        let mut ch = match Channel::open(&cols, label) {
            Ok(c) => c,
            Err(e) => {
                warn!("{label}: cannot open its HID++ channel: {e:#}");
                continue;
            }
        };
        let dev = match link {
            Link::KeyboardReceiver => match keyboard_slot(&mut ch) {
                Slot::Found(s) => s,
                Slot::Unknown if only_one => 1,
                // Another product's receiver (a mouse's, say): not ours.
                _ => continue,
            },
            _ => RECEIVER,
        };
        chosen = Some((ch, dev, path));
        break;
    }
    let (ch, dev, path) = chosen?;
    let mic = (link == Link::Headset).then(|| MicSetup {
        telephony: hidpp::sibling(all, &path, TELEPHONY_PAGE).cloned(),
        container: crate::audio::container_of_interface(&path)
            .map_err(|e| warn!("{label}: {e:#}"))
            .ok(),
    });
    let waker = ch.waker();
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let thread = std::thread::Builder::new()
        .name(format!("worker-{}", link.label()))
        .spawn(move || {
            info!("{}: worker started (device index {dev:#04x})", link.label());
            match link {
                Link::KeyboardReceiver | Link::KeyboardCable => keyboard(ch, link, dev, &rep, poll),
                Link::Headset => {
                    headset(ch, &rep, poll, buttons, mic.unwrap_or_default());
                    *taken() = None;
                }
            }
            rep.update(link, |s| s.state = State::Absent);
            info!("{}: worker stopped", link.label());
            done.store(true, Ordering::SeqCst);
        })
        .ok()?;
    Some(Worker { link, waker, finished, thread: Some(thread) })
}

/// A HID++ 2.0 event (software id 0) from `dev` on feature index `feat`.
fn event_payload(data: &[u8], dev: u8, feat: u8) -> Option<&[u8]> {
    (data.len() >= 7
        && data[0] == hidpp::LONG
        && data[1] == dev
        && data[2] == feat
        && data[3] & 0x0F == 0)
        .then(|| &data[4..])
}

/// Any unsolicited HID++ 2.0 event from `dev`: not an error, not a reply to
/// anyone's request. Proof the device is awake.
fn is_event_from(data: &[u8], dev: u8) -> bool {
    data.len() >= 7
        && data[0] == hidpp::LONG
        && data[1] == dev
        && data[2] != 0xFF
        && data[2] != 0x8F
        && data[3] & 0x0F == 0
}

/// How long to wait: until the fallback reading is due while the device is
/// awake, for ever while it sleeps.
fn wait_for(awake: bool, due: Option<Instant>, poll: Duration) -> Option<Duration> {
    awake.then(|| due.map(|d| d.saturating_duration_since(Instant::now())).unwrap_or(poll))
}

fn keyboard(mut ch: Channel, link: Link, dev: u8, rep: &Reporter, poll: Duration) {
    let name = link.label();
    let wireless = link == Link::KeyboardReceiver;
    if wireless {
        enable_link_notifications(&mut ch);
    }
    rep.update(link, |s| s.state = State::Sleeping);
    // Feature indices, looked up once the keyboard answers.
    let mut battery_idx: Option<u8> = None;
    let mut status_idx: Option<u8> = None;
    let mut connected = false;
    let mut need_read = true;
    let mut last_ok: Option<Instant> = None;
    let mut due: Option<Instant> = None;

    loop {
        if need_read {
            need_read = false;
            let read = (|| -> Result<Battery, HidppError> {
                let idx = match battery_idx {
                    Some(i) => i,
                    None => {
                        let i = ch
                            .feature_index(dev, feature::BATTERY_VOLTAGE)?
                            .ok_or_else(|| HidppError::Io("no BATTERY_VOLTAGE feature".into()))?
                            .0;
                        status_idx =
                            ch.feature_index(dev, feature::WIRELESS_DEVICE_STATUS)?.map(|f| f.0);
                        battery_idx = Some(i);
                        i
                    }
                };
                let p = ch.request(dev, idx, 0, &[])?;
                battery::from_g915(&p).ok_or_else(|| HidppError::Io("bad battery reply".into()))
            })();
            match read {
                Ok(b) => {
                    debug!("{name}: {} ({} mV)", b.short(), b.millivolts);
                    connected = true;
                    last_ok = Some(Instant::now());
                    due = Some(Instant::now() + poll);
                    rep.update(link, |s| {
                        s.state = State::Connected;
                        s.battery = Some(b);
                    });
                }
                // A sleeping keyboard is answered by the receiver: 0x09
                // "resource error", or 0x08 "unknown device" once the link is
                // down (the pairing itself is still there). The receiver
                // announces the link coming back, so just wait for that.
                Err(HidppError::Offline(_))
                | Err(HidppError::Error10(0x08))
                | Err(HidppError::Timeout) => {
                    debug!("{name}: asleep or out of range");
                    connected = false;
                    due = None;
                    rep.update(link, |s| s.state = State::Sleeping);
                }
                Err(HidppError::Io(e)) if e.contains("gone") => return,
                Err(e) => {
                    warn!("{name}: battery read failed: {e}");
                    if connected {
                        due = Some(Instant::now() + poll);
                    }
                }
            }
        }

        match ch.wait_event(wait_for(connected, due, poll)) {
            Err(e) => {
                info!("{name}: {e}");
                return;
            }
            Ok(None) => need_read = connected,
            Ok(Some(Event::Control(QUIT))) => return,
            Ok(Some(Event::Control(_))) => need_read = true,
            Ok(Some(Event::Report(r))) => {
                let d = &r.data;
                let just_read =
                    connected && last_ok.is_some_and(|t| t.elapsed() < Duration::from_secs(2));
                // HID++ 1.0 "device connection" from the receiver: flag bit 6
                // set means the link is down.
                if wireless && d.len() >= 7 && d[0] == hidpp::SHORT && d[1] == dev && d[2] == 0x41 {
                    if d[4] & 0x40 == 0 {
                        debug!("{name}: link up");
                        need_read = !just_read;
                    } else {
                        debug!("{name}: link down");
                        connected = false;
                        due = None;
                        rep.update(link, |s| s.state = State::Sleeping);
                    }
                } else if let Some(p) = battery_idx.and_then(|i| event_payload(d, dev, i)) {
                    if let Some(b) = battery::from_g915(p) {
                        debug!("{name}: battery event {} ({} mV)", b.short(), b.millivolts);
                        connected = true;
                        last_ok = Some(Instant::now());
                        due = Some(Instant::now() + poll);
                        rep.update(link, |s| {
                            s.state = State::Connected;
                            s.battery = Some(b);
                        });
                    }
                } else if status_idx.is_some_and(|i| event_payload(d, dev, i).is_some()) {
                    // Wireless device status: the keyboard (re)connected. It
                    // usually follows the receiver's link-up within a
                    // millisecond, which already caused a read.
                    need_read = !just_read;
                } else if !connected && is_event_from(d, dev) {
                    // Any event at all from a keyboard we think is asleep
                    // means it is not.
                    need_read = true;
                }
            }
        }
    }
}

/// The headset's telephony collection (usage page 0x0B): where its mute
/// button and boom report while blu-ghub has its buttons.
const TELEPHONY_PAGE: u16 = 0x000B;
/// That collection's one report: `08 xx`.
const TELEPHONY_REPORT: u8 = 0x08;
/// Its bits. Only the mute button (Phone Mute) is in the descriptor, beside
/// a Hook Switch the G935 has no use for; to Windows the boom's two bits are
/// padding, so it never sees them.
const MUTE_BUTTON: u8 = 0x01;
const BOOM_UP: u8 = 0x10;
const BOOM_DOWN: u8 = 0x20;

/// A failed takeover is tried again this often, a few times: the
/// microphone may not be in Windows' list yet just after the dongle is
/// plugged in.
const RETRY_AFTER: Duration = Duration::from_secs(5);
const RETRIES: u8 = 3;

/// What the headset asks of the microphone while blu-ghub has its buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MicRequest {
    /// The mute button.
    Toggle,
    /// The boom raised.
    Mute,
    /// The boom lowered.
    Unmute,
}

fn mic_request(bits: u8) -> Option<MicRequest> {
    if bits & BOOM_UP != 0 {
        Some(MicRequest::Mute)
    } else if bits & BOOM_DOWN != 0 {
        Some(MicRequest::Unmute)
    } else if bits & MUTE_BUTTON != 0 {
        Some(MicRequest::Toggle)
    } else {
        None
    }
}

/// What a report from the headset means.
#[derive(Debug, PartialEq)]
enum Route {
    /// Which G-keys are down (bit 0 = G1). Only sent while they are taken.
    Keys(u8),
    /// The mute button or the boom. Only sent while the G-keys are taken.
    Mic(MicRequest),
    Power(HeadsetPower),
    /// Any other event from the headset: proof it is on.
    Wake,
    Ignore,
}

fn route(d: &[u8], dev: u8, gkey: Option<u8>, adc: Option<u8>) -> Route {
    if d.len() == 2 && d[0] == TELEPHONY_REPORT {
        return mic_request(d[1]).map_or(Route::Ignore, Route::Mic);
    }
    if let Some(p) = gkey.and_then(|i| event_payload(d, dev, i)) {
        return Route::Keys(p[0]);
    }
    if let Some(p) = adc.and_then(|i| event_payload(d, dev, i)) {
        return battery::from_g935(p).map_or(Route::Ignore, Route::Power);
    }
    if is_event_from(d, dev) { Route::Wake } else { Route::Ignore }
}

/// What the headset worker needs to reach the microphone: the collection
/// the mute button and boom report on, and the headset's device container,
/// which its recording device in Windows shares.
#[derive(Default)]
pub struct MicSetup {
    pub telephony: Option<HidInfo>,
    pub container: Option<GUID>,
}

/// The headset's long HID++ handle and GKEY feature index while blu-ghub
/// has its buttons, for [`emergency_hand_back`].
static TAKEN: Mutex<Option<(isize, u8)>> = Mutex::new(None);

fn taken() -> std::sync::MutexGuard<'static, Option<(isize, u8)>> {
    TAKEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// For the panic hook: a release build aborts on panic, which would leave
/// the headset's buttons, mute button and boom with nobody to act on them,
/// so the microphone could not be unmuted. One write, no reply awaited.
pub fn emergency_hand_back() {
    let Ok(guard) = TAKEN.try_lock() else { return };
    if let Some((raw, feat)) = *guard {
        // Borrowed: the channel that owns the handle closes it.
        let handle = std::mem::ManuallyDrop::new(crate::hid::OwnedHandle(
            windows::Win32::Foundation::HANDLE(raw as *mut _),
        ));
        let frame = [hidpp::LONG, RECEIVER, feat, 0x20 | hidpp::SW_ID, 0x00];
        let _ = crate::hid::write_report(&handle, &frame, hidpp::LONG_LEN);
    }
}

/// The headset's G-keys and, while blu-ghub has them, its microphone mute.
struct Buttons {
    actions: [ButtonAction; 3],
    /// The telephony collection is being read.
    listening: bool,
    container: Option<GUID>,
    /// Feature index of 0x8010, looked up when first needed.
    feat: Option<u8>,
    /// The microphone in Windows, looked up when first needed.
    mic: Option<Mic>,
    diverted: bool,
    /// A takeover was sent this run: hand back on quit, whatever its reply.
    attempted: bool,
    /// Taking over failed this power-on. Tried again at `retry`, on a
    /// refresh or resume, and at the next power-on.
    failed: bool,
    retry: Option<Instant>,
    retries: u8,
    /// The hand-back that undoes a takeover left by a crashed or killed
    /// run has been sent (once a run, while blu-ghub is not taking over).
    cleared: bool,
    /// Which G-keys were down in the last report.
    held: u8,
    /// Windows refused the last key: warn once, not at every press.
    refused: bool,
}

impl Buttons {
    fn new(actions: [ButtonAction; 3], listening: bool, container: Option<GUID>) -> Self {
        Self {
            actions,
            listening,
            container,
            feat: None,
            mic: None,
            diverted: false,
            attempted: false,
            failed: false,
            retry: None,
            retries: 0,
            cleared: false,
            held: 0,
            refused: false,
        }
    }

    /// Something is configured, so the buttons are to be taken over.
    fn wanted(&self) -> bool {
        self.actions.iter().any(|a| *a != ButtonAction::Default)
    }

    /// Reports from the buttons are blu-ghub's to act on: it has them, or
    /// wants them and has not failed to take them (a takeover whose reply
    /// timed out may still have been applied).
    fn ours(&self) -> bool {
        self.diverted || (self.wanted() && !self.failed)
    }

    fn gkey(&mut self, ch: &mut Channel, dev: u8) -> anyhow::Result<u8> {
        if let Some(i) = self.feat {
            return Ok(i);
        }
        let i = ch
            .feature_index(dev, feature::GKEY)?
            .ok_or_else(|| anyhow!("the headset has no GKEY feature"))?
            .0;
        self.feat = Some(i);
        Ok(i)
    }

    fn mic(&mut self) -> anyhow::Result<&Mic> {
        let mic = match self.mic.take() {
            Some(m) => m,
            None => {
                let container =
                    self.container.ok_or_else(|| anyhow!("the headset's device is unknown"))?;
                Mic::find(container)
                    .context("looking for the headset's microphone")?
                    .ok_or_else(|| anyhow!("Windows lists no microphone for the headset"))?
            }
        };
        Ok(self.mic.insert(mic))
    }

    /// Called while the headset is on: take the buttons if that is wanted
    /// and blu-ghub can act on the mute button and boom that come with
    /// them; otherwise make sure the headset has them.
    fn settle(&mut self, ch: &mut Channel, dev: u8) {
        if self.diverted || self.failed {
            return;
        }
        if self.wanted() {
            match self.take(ch, dev) {
                Ok(()) => return,
                Err(e) => {
                    self.failed = true;
                    // Retrying cannot bring back a collection or a device
                    // container that was not there at the start.
                    let lasting = !self.listening || self.container.is_none();
                    if !lasting && self.retries < RETRIES {
                        self.retries += 1;
                        self.retry = Some(Instant::now() + RETRY_AFTER);
                        warn!("G935: not taking the buttons over: {e:#} (trying again shortly)");
                    } else {
                        warn!("G935: not taking the buttons over: {e:#}");
                    }
                }
            }
        }
        if !self.cleared {
            self.cleared = true;
            match self.hand_back(ch, dev) {
                Ok(()) => debug!("G935: the buttons are the headset's"),
                Err(e) => debug!("G935: could not make sure the headset has its buttons: {e:#}"),
            }
        }
    }

    fn take(&mut self, ch: &mut Channel, dev: u8) -> anyhow::Result<()> {
        // The mute button and the boom come with the buttons, and the
        // headset no longer mutes by itself: without a way to act on them
        // its microphone could be muted and never unmuted.
        if !self.listening {
            bail!("cannot read the headset's mute button");
        }
        self.mic()?;
        let feat = self.gkey(ch, dev)?;
        self.attempted = true;
        ch.request(dev, feat, 2, &[0x01])?;
        self.diverted = true;
        self.held = 0;
        *taken() = Some((ch.raw_long_handle(), feat));
        info!(
            "G935: buttons taken over ({:?}); the mute button and boom now mute the microphone \
             in Windows",
            self.actions
        );
        Ok(())
    }

    /// The headset reports its buttons although blu-ghub did not know it had
    /// them (a takeover whose reply timed out, or one it took for lost).
    fn adopt(&mut self, ch: &Channel) {
        if let (false, Some(feat)) = (self.diverted, self.feat) {
            info!("G935: the buttons were taken over after all");
            self.diverted = true;
            *taken() = Some((ch.raw_long_handle(), feat));
        }
    }

    /// Give the buttons, and the microphone mute, back to the headset.
    fn hand_back(&mut self, ch: &mut Channel, dev: u8) -> anyhow::Result<()> {
        self.diverted = false;
        *taken() = None;
        let feat = self.gkey(ch, dev)?;
        ch.request(dev, feat, 2, &[0x00])?;
        Ok(())
    }

    /// On quit.
    fn release(&mut self, ch: &mut Channel, dev: u8) {
        if self.attempted || self.diverted {
            match self.hand_back(ch, dev) {
                Ok(()) => info!("G935: buttons handed back"),
                Err(e) => warn!("G935: could not hand the buttons back: {e:#}"),
            }
        }
    }

    /// The headset switched off, which ends the takeover.
    fn forget(&mut self) {
        self.diverted = false;
        self.failed = false;
        self.retry = None;
        self.retries = 0;
        self.held = 0;
        *taken() = None;
    }

    /// Take the buttons again even if blu-ghub believes it has them: after
    /// a refresh or resume, or a power-on blu-ghub did not see as such, the
    /// headset may have forgotten. Taking over twice does no harm.
    fn invalidate(&mut self) {
        self.diverted = false;
        self.failed = false;
        self.retry = None;
        self.retries = 0;
    }

    /// A G-key report: the configured actions of the keys that went down.
    fn pressed(&mut self, mask: u8) -> Vec<(usize, ButtonAction)> {
        let down = mask & !self.held;
        self.held = mask;
        (0..self.actions.len())
            .filter(|b| down & (1 << b) != 0)
            .map(|b| (b, self.actions[b]))
            .collect()
    }

    fn run(&mut self, key: usize, action: ButtonAction) {
        debug!("G935: G{} -> {action:?}", key + 1);
        match action.run() {
            Ok(()) => self.refused = false,
            Err(e) if !self.refused => {
                self.refused = true;
                warn!(
                    "G935: Windows refused the key for G{} ({e}); keys cannot reach a window \
                     running as administrator, or a locked PC",
                    key + 1
                );
            }
            Err(_) => {}
        }
    }

    /// The mute button or the boom: mute or unmute the microphone in
    /// Windows. Returns whether it is now muted.
    fn on_mic(&mut self, req: MicRequest) -> anyhow::Result<bool> {
        match self.apply_mic(req) {
            Ok(muted) => Ok(muted),
            // Its endpoint may be stale (the dongle replugged, the device
            // switched off in Windows): look it up again, once.
            Err(_) => {
                self.mic = None;
                self.apply_mic(req)
            }
        }
    }

    fn apply_mic(&mut self, req: MicRequest) -> anyhow::Result<bool> {
        let mic = self.mic()?;
        match req {
            MicRequest::Toggle => mic.toggle(),
            MicRequest::Mute => mic.set(true).map(|()| true),
            MicRequest::Unmute => mic.unmute().map(|()| false),
        }
    }
}

fn headset(
    mut ch: Channel,
    rep: &Reporter,
    poll: Duration,
    actions: [ButtonAction; 3],
    setup: MicSetup,
) {
    let link = Link::Headset;
    let dev = RECEIVER;
    rep.update(link, |s| s.state = State::Sleeping);
    let _com = Com::init().map_err(|e| warn!("G935: COM: {e:#}")).ok();
    let listening = match &setup.telephony {
        Some(t) => {
            ch.listen(t).map_err(|e| warn!("G935: cannot read the mute button: {e:#}")).is_ok()
        }
        None => false,
    };
    let mut buttons = Buttons::new(actions, listening, setup.container);
    if buttons.wanted() {
        for (n, a) in actions.iter().enumerate() {
            if *a == ButtonAction::Default {
                warn!(
                    "G935: G{} = default does nothing while the other buttons are taken over",
                    n + 1
                );
            }
        }
    }
    let mut adc_idx: Option<u8> = None;
    let mut on = false;
    let mut due: Option<Instant> = None;
    let mut need_read = true;

    let apply = |power: HeadsetPower, on: &mut bool, due: &mut Option<Instant>| match power {
        HeadsetPower::On(b) => {
            debug!("G935: {} ({} mV)", b.short(), b.millivolts);
            *on = true;
            *due = Some(Instant::now() + poll);
            rep.update(link, |s| {
                s.state = State::Connected;
                s.battery = Some(b);
            });
        }
        HeadsetPower::Off => {
            debug!("G935: switched off");
            *on = false;
            *due = None;
            rep.update(link, |s| s.state = State::Sleeping);
        }
    };

    loop {
        if need_read {
            need_read = false;
            let read = (|| -> Result<HeadsetPower, HidppError> {
                let idx = match adc_idx {
                    Some(i) => i,
                    None => {
                        let i = ch
                            .feature_index(dev, feature::ADC_MEASUREMENT)?
                            .ok_or_else(|| HidppError::Io("no ADC_MEASUREMENT feature".into()))?
                            .0;
                        adc_idx = Some(i);
                        i
                    }
                };
                let p = ch.request(dev, idx, 0, &[])?;
                battery::from_g935(&p).ok_or_else(|| HidppError::Io("bad battery reply".into()))
            })();
            match read {
                Ok(p) => apply(p, &mut on, &mut due),
                Err(HidppError::Io(e)) if e.contains("gone") => return,
                // Busy means it is on and answering; a failed write is between
                // the PC and the dongle. Neither says the headset is off.
                Err(e @ HidppError::Error20(0x08)) | Err(e @ HidppError::Io(_)) => {
                    warn!("G935: battery read failed: {e}");
                    if on {
                        due = Some(Instant::now() + poll);
                    }
                }
                // A headset that is off answers with an error, or not at all.
                Err(e) => {
                    debug!("G935: no reading ({e}); treating as off");
                    apply(HeadsetPower::Off, &mut on, &mut due);
                }
            }
        }
        if on {
            buttons.settle(&mut ch, dev);
        } else {
            buttons.forget();
        }

        let mut timeout = wait_for(on, due, poll);
        if let Some(r) = buttons.retry {
            let left = r.saturating_duration_since(Instant::now());
            timeout = Some(timeout.map_or(left, |t| t.min(left)));
        }
        match ch.wait_event(timeout) {
            Err(e) => {
                info!("G935: {e}");
                return;
            }
            Ok(None) => {
                let now = Instant::now();
                if buttons.retry.is_some_and(|r| r <= now) {
                    buttons.retry = None;
                    buttons.failed = false;
                }
                need_read = on && due.is_none_or(|d| d <= now);
            }
            Ok(Some(Event::Control(QUIT))) => {
                buttons.release(&mut ch, dev);
                return;
            }
            Ok(Some(Event::Control(_))) => {
                buttons.invalidate();
                need_read = true;
            }
            Ok(Some(Event::Report(r))) => match route(&r.data, dev, buttons.feat, adc_idx) {
                Route::Keys(mask) => {
                    if buttons.ours() {
                        buttons.adopt(&ch);
                        for (key, action) in buttons.pressed(mask) {
                            buttons.run(key, action);
                        }
                    } else {
                        // Taken by a run that crashed, or after a reply that
                        // timed out, while blu-ghub cannot act on the mute.
                        info!("G935: buttons held by nobody; handing them back");
                        let _ = buttons.hand_back(&mut ch, dev);
                    }
                    need_read |= !on;
                }
                Route::Mic(req) => {
                    // Whoever took the buttons, somebody has to answer this.
                    let acted = match buttons.on_mic(req) {
                        Ok(muted) => {
                            info!("G935: microphone {}", if muted { "muted" } else { "on" });
                            true
                        }
                        Err(e) => {
                            warn!(
                                "G935: could not act on the {req:?} from the headset: {e:#}; \
                                 handing the buttons back so it mutes by itself"
                            );
                            buttons.failed = true;
                            false
                        }
                    };
                    if !acted || !buttons.ours() {
                        let _ = buttons.hand_back(&mut ch, dev);
                    }
                    need_read |= !on;
                }
                Route::Power(power) => {
                    // On while on: a power-on that never showed as off.
                    if on && matches!(power, HeadsetPower::On(_)) {
                        buttons.invalidate();
                    }
                    apply(power, &mut on, &mut due);
                }
                // Only the headset's own event says it is back; other
                // programs' replies and error reports do not.
                Route::Wake => need_read |= !on,
                Route::Ignore => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_events_from_the_right_device_and_feature_carry_a_payload() {
        let ev = [0x11, 0x01, 0x07, 0x00, 0x0F, 0xEE, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(event_payload(&ev, 0x01, 0x07).map(|p| &p[..3]), Some(&[0x0F, 0xEE, 0x00][..]));
        // A reply to someone's request (software id 9) is not an event.
        let reply = [0x11, 0x01, 0x07, 0x09, 0x0F, 0xEE, 0x00];
        assert_eq!(event_payload(&reply, 0x01, 0x07), None);
        // Another feature, another device.
        assert_eq!(event_payload(&ev, 0x01, 0x08), None);
        assert_eq!(event_payload(&ev, 0x02, 0x07), None);
    }

    #[test]
    fn errors_and_other_programs_replies_do_not_count_as_the_device_waking() {
        let event = [0x11, 0xFF, 0x08, 0x00, 0x0F, 0x09, 0x01];
        let reply = [0x11, 0xFF, 0x08, 0x1D, 0x0F, 0x09, 0x01];
        let error20 = [0x11, 0xFF, 0xFF, 0x08, 0x09, 0x05, 0x00];
        let error10 = [0x10, 0xFF, 0x8F, 0x08, 0x09, 0x09, 0x00];
        assert!(is_event_from(&event, 0xFF));
        assert!(!is_event_from(&reply, 0xFF));
        assert!(!is_event_from(&error20, 0xFF));
        assert!(!is_event_from(&error10, 0xFF));
    }

    #[test]
    fn a_sleeping_device_waits_for_ever_and_an_awake_one_until_due() {
        assert_eq!(wait_for(false, None, Duration::from_secs(60)), None);
        let w =
            wait_for(true, Some(Instant::now() + Duration::from_secs(30)), Duration::from_secs(60));
        assert!(w.is_some_and(|d| d <= Duration::from_secs(30) && d > Duration::from_secs(28)));
        // Overdue: no wait at all.
        let past = Instant::now() - Duration::from_secs(5);
        assert_eq!(wait_for(true, Some(past), Duration::from_secs(60)), Some(Duration::ZERO));
    }

    const ACTIONS: [ButtonAction; 3] =
        [ButtonAction::NextTrack, ButtonAction::PlayPause, ButtonAction::Mute];

    #[test]
    fn a_button_acts_once_when_pressed_not_while_held_or_on_release() {
        let mut b = Buttons::new(ACTIONS, true, None);
        assert_eq!(b.pressed(0b010), [(1, ButtonAction::PlayPause)]);
        assert_eq!(b.pressed(0b010), []);
        assert_eq!(b.pressed(0b011), [(0, ButtonAction::NextTrack)]);
        assert_eq!(b.pressed(0b000), []);
        assert_eq!(
            b.pressed(0b111),
            [(0, ButtonAction::NextTrack), (1, ButtonAction::PlayPause), (2, ButtonAction::Mute)]
        );
    }

    #[test]
    fn bits_beyond_the_three_buttons_are_ignored() {
        let mut b = Buttons::new(ACTIONS, true, None);
        assert_eq!(b.pressed(0b1000), []);
    }

    #[test]
    fn buttons_left_to_the_firmware_are_not_taken_over() {
        assert!(!Buttons::new([ButtonAction::Default; 3], true, None).wanted());
        let one = [ButtonAction::Default, ButtonAction::PlayPause, ButtonAction::Default];
        assert!(Buttons::new(one, true, None).wanted());
    }

    #[test]
    fn button_reports_are_ours_only_while_wanted_and_not_failed() {
        let mut b = Buttons::new(ACTIONS, true, None);
        assert!(b.ours());
        b.failed = true;
        assert!(!b.ours());
        b.diverted = true;
        assert!(b.ours());
        assert!(!Buttons::new([ButtonAction::Default; 3], true, None).ours());
    }

    #[test]
    fn the_mute_button_toggles_and_the_boom_sets() {
        assert_eq!(mic_request(MUTE_BUTTON), Some(MicRequest::Toggle));
        assert_eq!(mic_request(BOOM_UP), Some(MicRequest::Mute));
        assert_eq!(mic_request(BOOM_DOWN), Some(MicRequest::Unmute));
        // Hook Switch, which the G935 has no use for.
        assert_eq!(mic_request(0x02), None);
    }

    fn long(bytes: &[u8]) -> Vec<u8> {
        let mut r = bytes.to_vec();
        r.resize(20, 0);
        r
    }

    #[test]
    fn headset_reports_are_routed_by_collection_and_feature() {
        let (gkey, adc) = (Some(5), Some(8));
        assert_eq!(route(&long(&[0x11, 0xFF, 5, 0x00, 0x04]), 0xFF, gkey, adc), Route::Keys(4));
        assert_eq!(route(&[0x08, 0x20], 0xFF, gkey, adc), Route::Mic(MicRequest::Unmute));
        assert_eq!(
            route(&long(&[0x11, 0xFF, 8, 0x00, 0, 0, 0]), 0xFF, gkey, adc),
            Route::Power(HeadsetPower::Off)
        );
        assert_eq!(route(&long(&[0x11, 0xFF, 4, 0x00, 1]), 0xFF, gkey, adc), Route::Wake);
    }

    #[test]
    fn a_reply_to_a_takeover_is_not_a_key_press() {
        // fn2 | software id 9, echoing 01.
        let r = long(&[0x11, 0xFF, 5, 0x29, 0x01]);
        assert_eq!(route(&r, 0xFF, Some(5), Some(8)), Route::Ignore);
    }

    #[test]
    fn the_cable_wins_over_the_receiver_while_plugged_in() {
        let mut s = Status::default();
        s.keyboard_receiver.state = State::Sleeping;
        assert_eq!(s.device(Device::Keyboard).state, State::Sleeping);
        s.keyboard_cable.state = State::Connected;
        assert_eq!(s.device(Device::Keyboard).state, State::Connected);
    }
}
