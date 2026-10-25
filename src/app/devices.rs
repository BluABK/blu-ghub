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

use crate::battery::{self, Battery, HeadsetPower};
use crate::hid::HidInfo;
use crate::hidpp::{self, Channel, Event, HidppError, RECEIVER, Waker, feature};

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
        self.waker.wake(QUIT);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
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
pub fn start(link: Link, all: &[HidInfo], rep: Reporter, poll: Duration) -> Option<Worker> {
    let (pid, label) = match link {
        Link::KeyboardReceiver => (0xC547, "G915 receiver"),
        Link::KeyboardCable => (0xC33E, "G915 cable"),
        Link::Headset => (0x0A87, "G935"),
    };
    let groups = hidpp::hidpp_groups(all, pid);
    let only_one = groups.len() == 1;
    let mut chosen = None;
    for cols in groups {
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
        chosen = Some((ch, dev));
        break;
    }
    let (ch, dev) = chosen?;
    let waker = ch.waker();
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let thread = std::thread::Builder::new()
        .name(format!("worker-{}", link.label()))
        .spawn(move || {
            info!("{}: worker started (device index {dev:#04x})", link.label());
            match link {
                Link::KeyboardReceiver | Link::KeyboardCable => keyboard(ch, link, dev, &rep, poll),
                Link::Headset => headset(ch, &rep, poll),
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

fn headset(mut ch: Channel, rep: &Reporter, poll: Duration) {
    let link = Link::Headset;
    let dev = RECEIVER;
    rep.update(link, |s| s.state = State::Sleeping);
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

        match ch.wait_event(wait_for(on, due, poll)) {
            Err(e) => {
                info!("G935: {e}");
                return;
            }
            Ok(None) => need_read = on,
            Ok(Some(Event::Control(QUIT))) => return,
            Ok(Some(Event::Control(_))) => need_read = true,
            Ok(Some(Event::Report(r))) => {
                let d = &r.data;
                match adc_idx.and_then(|i| event_payload(d, dev, i)) {
                    Some(p) => {
                        if let Some(power) = battery::from_g935(p) {
                            apply(power, &mut on, &mut due);
                        }
                    }
                    // Only the headset's own event says it is back; other
                    // programs' replies and error reports do not.
                    None if !on && is_event_from(d, dev) => need_read = true,
                    None => {}
                }
            }
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

    #[test]
    fn the_cable_wins_over_the_receiver_while_plugged_in() {
        let mut s = Status::default();
        s.keyboard_receiver.state = State::Sleeping;
        assert_eq!(s.device(Device::Keyboard).state, State::Sleeping);
        s.keyboard_cable.state = State::Connected;
        assert_eq!(s.device(Device::Keyboard).state, State::Connected);
    }
}
