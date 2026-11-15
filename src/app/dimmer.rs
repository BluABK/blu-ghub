//! Dimming blu-ghub's layout when idle, and the keyboard's own lighting
//! timers.
//!
//! Idle is Windows' last input (GetLastInputInfo: any keyboard or mouse,
//! not a hook), looked at once when the dim would be due and four times a
//! second only while dimmed. The fade is the layout painted again at lower
//! levels, into the keyboard's RAM like any frame.

use std::time::{Duration, Instant};

use tracing::debug;

use crate::config::{Dim, KeyboardPower};
use crate::hidpp::{Channel, feature};

/// How often the fade paints (at most: a frame takes what it takes).
const FRAME: Duration = Duration::from_millis(100);
/// How often input is looked for while dimmed.
const POLL: Duration = Duration::from_millis(250);

/// How long since the last keyboard or mouse input anywhere in the
/// session.
pub fn idle_for() -> Duration {
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: `info` is a valid LASTINPUTINFO with its size set.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return Duration::ZERO;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount() };
    Duration::from_millis(now.wrapping_sub(info.dwTime) as u64)
}

/// Where the dimming is: the level to show (1 = the layout as it is), and
/// when to look again.
pub struct Dimmer {
    dim: Option<Dim>,
    pub level: f32,
    fade_start: Option<Instant>,
    next: Option<Instant>,
}

impl Dimmer {
    pub fn new(dim: Option<Dim>) -> Self {
        Dimmer { dim, level: 1.0, fade_start: None, next: None }
    }

    pub fn set(&mut self, dim: Option<Dim>) {
        self.dim = dim;
        self.next = None;
    }

    /// When `tick` wants to run next; `None`: not until `start`.
    pub fn due(&self) -> Option<Instant> {
        self.next
    }

    /// The keyboard went away or to sleep: nothing to time until it is back.
    pub fn stop(&mut self) {
        self.level = 1.0;
        self.fade_start = None;
        self.next = None;
    }

    /// Something was pressed that Windows does not count as input (a G-key
    /// or M-key taken by blu-ghub): bright again. Returns the new level if
    /// it changed.
    pub fn activity(&mut self, now: Instant) -> Option<f32> {
        self.fade_start = None;
        if let Some(d) = self.dim {
            self.next = Some(now + Duration::from_secs(d.after.into()));
        }
        self.bright()
    }

    fn bright(&mut self) -> Option<f32> {
        (self.level != 1.0).then(|| {
            self.level = 1.0;
            1.0
        })
    }

    /// Look again, `idle` since the last input: the new level if it changed.
    pub fn tick(&mut self, idle: Duration, now: Instant) -> Option<f32> {
        let Some(d) = self.dim else {
            self.next = None;
            self.fade_start = None;
            return self.bright();
        };
        let after = Duration::from_secs(d.after.into());
        if idle < after {
            self.next = Some(now + (after - idle));
            self.fade_start = None;
            return self.bright();
        }
        let start = *self.fade_start.get_or_insert(now);
        let t = if d.fade <= 0.0 {
            1.0
        } else {
            (now.saturating_duration_since(start).as_secs_f32() / d.fade).min(1.0)
        };
        let target = f32::from(d.to.min(100)) / 100.0;
        // Down in 1/64 steps: finer is not seen, and every step is a frame.
        let level = ((1.0 - (1.0 - target) * t) * 64.0).round() / 64.0;
        self.next = Some(now + if t < 1.0 { FRAME } else { POLL });
        ((level - self.level).abs() > f32::EPSILON).then(|| {
            self.level = level;
            level
        })
    }
}

/// Send the keyboard its dim and lights-off times, if the settings have
/// either: read first, and written only if different (the keyboard keeps
/// them in RAM, so this is due at every connect and wake).
pub fn set_timers(ch: &mut Channel, dev: u8, power: &KeyboardPower, name: &str) {
    if power.dim.is_none() && power.lights_off_after.is_none() {
        return;
    }
    let fx = match ch.feature_index(dev, feature::RGB_EFFECTS) {
        Ok(Some((f, _, _))) => f,
        Ok(None) => return,
        Err(e) => {
            debug!("{name}: lighting timers: {e}");
            return;
        }
    };
    let now = match ch.request(dev, fx, 7, &[0x00]) {
        Ok(r) if r.len() >= 7 => {
            (u16::from_be_bytes([r[3], r[4]]), u16::from_be_bytes([r[5], r[6]]))
        }
        Ok(_) => return,
        Err(e) => {
            debug!("{name}: reading the lighting timers: {e}");
            return;
        }
    };
    let want = timers(power, now);
    if want == now {
        return;
    }
    let ([ih, il], [sh, sl]) = (want.0.to_be_bytes(), want.1.to_be_bytes());
    match ch.request(dev, fx, 7, &[0x01, 0x00, 0x00, ih, il, sh, sl]) {
        Ok(_) => debug!(
            "{name}: lighting timers {} s / {} s, were {} s / {} s",
            want.0, want.1, now.0, now.1
        ),
        Err(e) => debug!("{name}: setting the lighting timers: {e}"),
    }
}

/// (dim, lights off) in seconds for the keyboard, from the settings and
/// what it has now: the dim no later than the lights going off.
fn timers(power: &KeyboardPower, now: (u16, u16)) -> (u16, u16) {
    let off = power.lights_off_after.map_or(now.1, |s| s.min(u16::MAX.into()) as u16);
    let dim = power.dim.map_or(now.0, |d| d.after.min(u16::MAX.into()) as u16);
    (dim.min(off), off)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: Dim = Dim { after: 60, fade: 2.0, to: 25 };

    #[test]
    fn it_waits_until_idle_then_fades_and_brightens_on_input() {
        let t0 = Instant::now();
        let mut d = Dimmer::new(Some(DIM));
        // 20 s idle: look again in 40 s.
        assert_eq!(d.tick(Duration::from_secs(20), t0), None);
        assert_eq!(d.due(), Some(t0 + Duration::from_secs(40)));
        // Due: the fade starts at full, a frame later it is lower.
        let t1 = t0 + Duration::from_secs(40);
        assert_eq!(d.tick(Duration::from_secs(60), t1), None);
        assert_eq!(d.due(), Some(t1 + FRAME));
        let half = d.tick(Duration::from_secs(61), t1 + Duration::from_secs(1)).unwrap();
        assert!((half - 0.625).abs() < 0.02, "{half}");
        // Done: at the target, looked at four times a second.
        let t2 = t1 + Duration::from_secs(3);
        assert_eq!(d.tick(Duration::from_secs(63), t2), Some(0.25));
        assert_eq!(d.tick(Duration::from_secs(63), t2), None);
        assert_eq!(d.due(), Some(t2 + POLL));
        // Input: full at once, and the wait starts over.
        assert_eq!(d.tick(Duration::from_millis(100), t2), Some(1.0));
        assert_eq!(d.due(), Some(t2 + Duration::from_millis(59_900)));
    }

    #[test]
    fn no_fade_goes_straight_down_and_a_g_key_brings_it_back() {
        let t0 = Instant::now();
        let mut d = Dimmer::new(Some(Dim { fade: 0.0, to: 0, ..DIM }));
        assert_eq!(d.tick(Duration::from_secs(70), t0), Some(0.0));
        assert_eq!(d.activity(t0), Some(1.0));
        assert_eq!(d.due(), Some(t0 + Duration::from_secs(60)));
        // Switched off while dim: bright, and no more looking.
        d.tick(Duration::from_secs(70), t0);
        d.set(None);
        assert_eq!(d.tick(Duration::from_secs(70), t0), Some(1.0));
        assert_eq!(d.due(), None);
    }

    #[test]
    fn timers_keep_what_is_not_set_and_dim_before_off() {
        let none = KeyboardPower::default();
        assert_eq!(timers(&none, (60, 300)), (60, 300));
        let off = KeyboardPower { lights_off_after: Some(120), ..none };
        assert_eq!(timers(&off, (60, 300)), (60, 120));
        let both =
            KeyboardPower { dim: Some(Dim { after: 600, ..DIM }), lights_off_after: Some(120) };
        assert_eq!(timers(&both, (60, 300)), (120, 120));
    }
}
