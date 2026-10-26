//! Battery voltage to charge, for both devices.
//!
//! Neither device reports a percentage: the G915 (feature 0x1001) and the
//! G935 (0x1F20) report the cell voltage and a status byte, and the host
//! turns the voltage into a percentage with a discharge curve. Li-ion voltage
//! is flat through most of the discharge, so the curve matters more than the
//! arithmetic. While charging the voltage reads high, so the percentage is
//! shown as an estimate.

use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq)]
pub struct Battery {
    pub percent: u8,
    pub millivolts: u16,
    pub charging: bool,
    pub full: bool,
    pub at: SystemTime,
}

impl Battery {
    /// "71%", "71% charging", "full".
    pub fn short(&self) -> String {
        if self.full {
            "full".into()
        } else if self.charging {
            format!("{}% charging", self.percent)
        } else {
            format!("{}%", self.percent)
        }
    }

    /// [`Battery::short`] with a battery symbol in front, where people
    /// read it (the logs keep the plain text).
    pub fn shown(&self) -> String {
        format!("\u{1F50B} {}", self.short())
    }
}

/// Linear interpolation over a (millivolts, percent) table sorted by
/// descending voltage. Clamped at both ends.
fn interpolate(table: &[(u16, f32)], mv: u16) -> u8 {
    let (top_mv, top_pct) = table[0];
    if mv >= top_mv {
        return top_pct.round() as u8;
    }
    for w in table.windows(2) {
        let ((hi_mv, hi_pct), (lo_mv, lo_pct)) = (w[0], w[1]);
        if mv >= lo_mv {
            let frac = (mv - lo_mv) as f32 / (hi_mv - lo_mv) as f32;
            return (lo_pct + frac * (hi_pct - lo_pct)).round() as u8;
        }
    }
    table[table.len() - 1].1.round() as u8
}

/// G915: the curve Solaar uses for Logitech's BATTERY_VOLTAGE devices.
const G915_CURVE: &[(u16, f32)] = &[
    (4186, 100.0),
    (4067, 90.0),
    (3989, 80.0),
    (3922, 70.0),
    (3859, 60.0),
    (3811, 50.0),
    (3778, 40.0),
    (3751, 30.0),
    (3717, 20.0),
    (3671, 10.0),
    (3646, 5.0),
    (3579, 2.0),
    (3500, 0.0),
];

/// G935: remaining energy against voltage, from the headset's discharge
/// curve (sampled every 5 %; the one non-monotonic point near 10 % left out).
const G935_CURVE: &[(u16, f32)] = &[
    (4057, 100.0),
    (3979, 95.0),
    (3933, 90.0),
    (3884, 85.0),
    (3837, 80.0),
    (3803, 75.0),
    (3768, 70.0),
    (3734, 65.0),
    (3705, 60.0),
    (3681, 55.0),
    (3660, 50.0),
    (3643, 45.0),
    (3631, 40.0),
    (3620, 35.0),
    (3609, 30.0),
    (3597, 25.0),
    (3581, 20.0),
    (3557, 15.0),
    (3547, 5.0),
    (3399, 0.0),
];

/// Decode the G915's 0x1001 reply or event: `[mV hi, mV lo, flags]`.
/// Flag bit 7: on external power; then the low bits say 0 charging,
/// 1 full, 2 not charging.
pub fn from_g915(p: &[u8]) -> Option<Battery> {
    if p.len() < 3 {
        return None;
    }
    let mv = u16::from_be_bytes([p[0], p[1]]);
    if mv == 0 {
        return None;
    }
    let external = p[2] & 0x80 != 0;
    let status = p[2] & 0x07;
    Some(Battery {
        percent: interpolate(G915_CURVE, mv),
        millivolts: mv,
        charging: external && status == 0,
        full: external && status == 1,
        at: SystemTime::now(),
    })
}

/// What the G935's 0x1F20 reply or event says.
#[derive(Debug, Clone, PartialEq)]
pub enum HeadsetPower {
    On(Battery),
    /// `00 00 00`: the headset turned off (the dongle stays plugged in).
    Off,
}

/// Decode the G935's 0x1F20 reply or event: `[mV hi, mV lo, flags]`, flags
/// 01 discharging, 03 charging, 07 full.
pub fn from_g935(p: &[u8]) -> Option<HeadsetPower> {
    if p.len() < 3 {
        return None;
    }
    let mv = u16::from_be_bytes([p[0], p[1]]);
    if mv == 0 && p[2] == 0 {
        return Some(HeadsetPower::Off);
    }
    if mv == 0 {
        return None;
    }
    Some(HeadsetPower::On(Battery {
        percent: interpolate(G935_CURVE, mv),
        millivolts: mv,
        charging: p[2] == 0x03,
        full: p[2] == 0x07,
        at: SystemTime::now(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curves_are_monotonic() {
        for curve in [G915_CURVE, G935_CURVE] {
            for w in curve.windows(2) {
                assert!(w[0].0 > w[1].0 && w[0].1 > w[1].1, "{w:?}");
            }
        }
    }

    #[test]
    fn interpolation_hits_the_points_and_clamps_the_ends() {
        assert_eq!(interpolate(G915_CURVE, 3811), 50);
        assert_eq!(interpolate(G915_CURVE, 5000), 100);
        assert_eq!(interpolate(G915_CURVE, 3000), 0);
        // Halfway between 3811 (50 %) and 3859 (60 %).
        assert_eq!(interpolate(G915_CURVE, 3835), 55);
    }

    #[test]
    fn a_g915_on_battery_is_not_charging() {
        let b = from_g915(&[0x0F, 0xEE, 0x00]).unwrap();
        assert_eq!(b.millivolts, 4078);
        assert!(!b.charging && !b.full);
        assert_eq!(b.percent, 91);
    }

    #[test]
    fn a_g915_on_its_cable_is_charging_or_full() {
        assert!(from_g915(&[0x0F, 0x9E, 0x80]).unwrap().charging);
        assert!(from_g915(&[0x10, 0x5A, 0x81]).unwrap().full);
    }

    #[test]
    fn the_g935_reads_as_ghub_did_and_reports_switching_off() {
        // 4004 mV discharging: G HUB showed 97 %.
        match from_g935(&[0x0F, 0xA4, 0x01]) {
            Some(HeadsetPower::On(b)) => assert_eq!(b.percent, 97),
            other => panic!("{other:?}"),
        }
        assert_eq!(from_g935(&[0, 0, 0]), Some(HeadsetPower::Off));
        match from_g935(&[0x0F, 0x09, 0x03]) {
            Some(HeadsetPower::On(b)) => assert!(b.charging),
            other => panic!("{other:?}"),
        }
    }
}
