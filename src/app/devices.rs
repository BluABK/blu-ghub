//! One worker thread per device, each owning its HID++ channel.
//!
//! A worker spends its life blocked in [`Channel::wait_event`]: it wakes when
//! the device sends something (a battery change, the keyboard waking or going
//! to sleep, the headset switching off), when the tray asks for a refresh, or
//! when the fallback interval passes. It never polls on a short timer, and it
//! sends nothing while the device is asleep or off.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Headset,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Keyboard => "G915",
            Kind::Headset => "G935",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Link {
    /// No receiver or dongle plugged in.
    Absent,
    /// The receiver or dongle is there, the device is asleep, off or out of
    /// range.
    Sleeping,
    Connected,
}

/// What the tray shows for one device.
#[derive(Debug, Clone)]
pub struct DeviceStatus {
    pub link: Link,
    /// The last reading, kept while the device sleeps.
    pub battery: Option<Battery>,
}

impl Default for DeviceStatus {
    fn default() -> Self {
        Self { link: Link::Absent, battery: None }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub keyboard: DeviceStatus,
    pub headset: DeviceStatus,
}

impl Status {
    pub fn get(&self, kind: Kind) -> &DeviceStatus {
        match kind {
            Kind::Keyboard => &self.keyboard,
            Kind::Headset => &self.headset,
        }
    }
    fn get_mut(&mut self, kind: Kind) -> &mut DeviceStatus {
        match kind {
            Kind::Keyboard => &mut self.keyboard,
            Kind::Headset => &mut self.headset,
        }
    }
}

pub type Shared = Arc<Mutex<Status>>;

/// A running worker.
pub struct Worker {
    pub kind: Kind,
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
    /// Ask the worker to stop and wait for it (it closes its channel).
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
    fn update(&self, kind: Kind, f: impl FnOnce(&mut DeviceStatus)) {
        {
            let mut s = self.shared.lock().unwrap();
            f(s.get_mut(kind));
        }
        unsafe {
            let _ =
                PostMessageW(Some(HWND(self.hwnd as *mut _)), WM_APP_STATE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Start a worker for `kind` if its receiver or dongle is among `all`.
pub fn start(kind: Kind, all: &[HidInfo], rep: Reporter, poll: Duration) -> Option<Worker> {
    let pid = match kind {
        Kind::Keyboard => 0xC547,
        Kind::Headset => 0x0A87,
    };
    let cols = hidpp::hidpp_collections(all, pid);
    if cols.is_empty() {
        return None;
    }
    let ch = match Channel::open(&cols, kind.name()) {
        Ok(c) => c,
        Err(e) => {
            warn!("{}: cannot open its HID++ channel: {e:#}", kind.name());
            return None;
        }
    };
    let waker = ch.waker();
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let thread = std::thread::Builder::new()
        .name(format!("worker-{}", kind.name()))
        .spawn(move || {
            info!("{}: worker started", kind.name());
            match kind {
                Kind::Keyboard => keyboard(ch, &rep, poll),
                Kind::Headset => headset(ch, &rep, poll),
            }
            rep.update(kind, |s| s.link = Link::Absent);
            info!("{}: worker stopped", kind.name());
            done.store(true, Ordering::SeqCst);
        })
        .ok()?;
    Some(Worker { kind, waker, finished, thread: Some(thread) })
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

/// The receiver's slot for the G915 (wireless product id 0x407C), from its
/// pairing register. Slot 1 if the register cannot be read.
fn keyboard_slot(ch: &mut Channel) -> u8 {
    for slot in 0..6u8 {
        if let Ok(p) = ch.read_register(RECEIVER, 0xB5, true, [0x20 + slot, 0, 0])
            && p.len() >= 5
            && p[3] == 0x40
            && p[4] == 0x7C
        {
            return slot + 1;
        }
    }
    1
}

fn keyboard(mut ch: Channel, rep: &Reporter, poll: Duration) {
    let kind = Kind::Keyboard;
    let dev = keyboard_slot(&mut ch);
    debug!("G915: receiver slot {dev}");
    rep.update(kind, |s| s.link = Link::Sleeping);
    // Feature indices, looked up once the keyboard answers.
    let mut battery_idx: Option<u8> = None;
    let mut status_idx: Option<u8> = None;
    let mut connected = false;
    let mut need_read = true;
    let mut last_read: Option<Instant> = None;

    loop {
        if need_read {
            need_read = false;
            last_read = Some(Instant::now());
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
                    debug!("G915: {} ({} mV)", b.short(), b.millivolts);
                    connected = true;
                    rep.update(kind, |s| {
                        s.link = Link::Connected;
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
                    debug!("G915: asleep or out of range");
                    connected = false;
                    rep.update(kind, |s| s.link = Link::Sleeping);
                }
                Err(HidppError::Io(e)) if e.contains("gone") => return,
                Err(e) => warn!("G915: battery read failed: {e}"),
            }
        }

        // Only a connected keyboard is ever polled; a sleeping one is left
        // alone until it says it is back.
        let wait = if connected { poll } else { Duration::from_secs(24 * 3600) };
        match ch.wait_event(wait) {
            Err(e) => {
                info!("G915: {e}");
                return;
            }
            Ok(None) => need_read = true,
            Ok(Some(Event::Control(QUIT))) => return,
            Ok(Some(Event::Control(_))) => need_read = true,
            Ok(Some(Event::Report(r))) => {
                let d = &r.data;
                // HID++ 1.0 "device connection" from the receiver: flag bit 6
                // set means the link is down.
                if d.len() >= 7 && d[0] == hidpp::SHORT && d[1] == dev && d[2] == 0x41 {
                    if d[4] & 0x40 == 0 {
                        debug!("G915: link up");
                        need_read = true;
                    } else {
                        debug!("G915: link down");
                        connected = false;
                        rep.update(kind, |s| s.link = Link::Sleeping);
                    }
                } else if let Some(p) = battery_idx.and_then(|i| event_payload(d, dev, i)) {
                    if let Some(b) = battery::from_g915(p) {
                        debug!("G915: battery event {} ({} mV)", b.short(), b.millivolts);
                        connected = true;
                        rep.update(kind, |s| {
                            s.link = Link::Connected;
                            s.battery = Some(b);
                        });
                    }
                } else if status_idx.is_some_and(|i| event_payload(d, dev, i).is_some()) {
                    // Wireless device status: the keyboard (re)connected. It
                    // usually comes right after the receiver's link-up, which
                    // already caused a read.
                    if last_read.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) {
                        need_read = true;
                    }
                }
            }
        }
    }
}

fn headset(mut ch: Channel, rep: &Reporter, poll: Duration) {
    let kind = Kind::Headset;
    let dev = RECEIVER;
    rep.update(kind, |s| s.link = Link::Sleeping);
    let mut adc_idx: Option<u8> = None;
    let mut on = false;
    let mut need_read = true;

    let apply = |power: HeadsetPower, on: &mut bool| match power {
        HeadsetPower::On(b) => {
            debug!("G935: {} ({} mV)", b.short(), b.millivolts);
            *on = true;
            rep.update(kind, |s| {
                s.link = Link::Connected;
                s.battery = Some(b);
            });
        }
        HeadsetPower::Off => {
            debug!("G935: switched off");
            *on = false;
            rep.update(kind, |s| s.link = Link::Sleeping);
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
                Ok(p) => apply(p, &mut on),
                // A headset that is off answers with an error, or not at all.
                Err(HidppError::Io(e)) if e.contains("gone") => return,
                Err(e) => {
                    debug!("G935: no reading ({e}); treating as off");
                    apply(HeadsetPower::Off, &mut on);
                }
            }
        }

        let wait = if on { poll } else { Duration::from_secs(24 * 3600) };
        match ch.wait_event(wait) {
            Err(e) => {
                info!("G935: {e}");
                return;
            }
            Ok(None) => need_read = true,
            Ok(Some(Event::Control(QUIT))) => return,
            Ok(Some(Event::Control(_))) => need_read = true,
            Ok(Some(Event::Report(r))) => {
                match adc_idx.and_then(|i| event_payload(&r.data, dev, i)) {
                    Some(p) => {
                        if let Some(power) = battery::from_g935(p) {
                            apply(power, &mut on);
                        }
                    }
                    // Anything else from a headset we think is off means it
                    // is back: read it properly.
                    None if !on => need_read = true,
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
}
