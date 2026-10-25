//! The G935's lighting: two zones (the logo and the side strips), each with
//! an effect from the headset's own list (feature 0x8070,
//! COLOR_LED_EFFECTS).
//!
//! The headset cannot store an effect (its zones report no persistency),
//! so a configured effect is written at every power-on. A zone that is not
//! configured is never written: it shows whatever the headset does by
//! itself.

use std::fmt;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::hidpp::{Channel, feature};

/// A colour, written `"#rrggbb"` in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Rgb(pub [u8; 3]);

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [r, g, b] = self.0;
        write!(f, "#{r:02x}{g:02x}{b:02x}")
    }
}

impl std::str::FromStr for Rgb {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        let hex = s.trim().trim_start_matches('#');
        let bad = || format!("{s:?} is not a colour like \"#00b6ff\"");
        if hex.len() != 6 {
            return Err(bad());
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| bad());
        Ok(Rgb([byte(0)?, byte(2)?, byte(4)?]))
    }
}

impl Serialize for Rgb {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    #[default]
    Off,
    Static,
    Breathing,
    Cycle,
}

impl Effect {
    pub const ALL: [Effect; 4] = [Effect::Off, Effect::Static, Effect::Breathing, Effect::Cycle];

    pub fn label(self) -> &'static str {
        match self {
            Effect::Off => "Off",
            Effect::Static => "Steady",
            Effect::Breathing => "Breathing",
            Effect::Cycle => "Colour cycle",
        }
    }

    /// The effect's id in the headset's list (looked up there to find its
    /// index, which is what is sent).
    fn id(self) -> u16 {
        match self {
            Effect::Off => 0x0000,
            Effect::Static => 0x0001,
            Effect::Cycle => 0x0003,
            Effect::Breathing => 0x000A,
        }
    }

    pub fn has_color(self) -> bool {
        matches!(self, Effect::Static | Effect::Breathing)
    }

    pub fn has_period(self) -> bool {
        matches!(self, Effect::Breathing | Effect::Cycle)
    }
}

/// One zone's effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ZoneLight {
    pub effect: Effect,
    pub color: Rgb,
    /// One breath or one trip round the colours. The field is 16-bit
    /// milliseconds; what the headset makes of the extremes is its own
    /// business.
    pub period_ms: u16,
    /// 1-100. The steady effect has no brightness of its own, so for it the
    /// colour is dimmed instead.
    pub brightness: u8,
}

impl Default for ZoneLight {
    fn default() -> Self {
        Self { effect: Effect::Off, color: Rgb([255, 255, 255]), period_ms: 4000, brightness: 100 }
    }
}

impl ZoneLight {
    pub const PERIOD_MS: std::ops::RangeInclusive<u16> = 1..=u16::MAX;

    pub fn sanitized(mut self) -> Self {
        self.period_ms = self.period_ms.clamp(*Self::PERIOD_MS.start(), *Self::PERIOD_MS.end());
        self.brightness = self.brightness.clamp(1, 100);
        self
    }

    /// The ten parameter bytes of a set-effect request, laid out as Solaar
    /// and HeadsetControl send them: steady `[r g b ramp]`, breathing
    /// `[r g b period(2) form intensity]`, cycle
    /// `[0 0 0 0 0 period(2) intensity]`.
    pub fn params(&self) -> [u8; 10] {
        let s = self.sanitized();
        let [r, g, b] = s.color.0;
        let [ph, pl] = s.period_ms.to_be_bytes();
        match s.effect {
            Effect::Off => [0; 10],
            Effect::Static => {
                let dim = |c: u8| (c as u16 * s.brightness as u16 / 100) as u8;
                [dim(r), dim(g), dim(b), 0, 0, 0, 0, 0, 0, 0]
            }
            Effect::Breathing => [r, g, b, ph, pl, 0, s.brightness, 0, 0, 0],
            Effect::Cycle => [0, 0, 0, 0, 0, ph, pl, s.brightness, 0, 0],
        }
    }
}

/// The headset's two zones as the config names them. Absent: left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeadsetLighting {
    /// The strips round the ear cups (zone 1).
    pub sides: Option<ZoneLight>,
    /// The G logo (zone 0).
    pub logo: Option<ZoneLight>,
}

impl HeadsetLighting {
    pub fn is_set(&self) -> bool {
        self.sides.is_some() || self.logo.is_some()
    }

    /// Strips first, then the logo, the order G HUB uses.
    fn zones(&self) -> [(u8, Option<ZoneLight>); 2] {
        [(1, self.sides), (0, self.logo)]
    }
}

/// Write the configured zones to the headset: volatile, so at every
/// power-on. `feat` caches the feature index between calls.
pub fn apply(
    ch: &mut Channel,
    dev: u8,
    lighting: &HeadsetLighting,
    feat: &mut Option<u8>,
) -> Result<()> {
    let i = match *feat {
        Some(i) => i,
        None => {
            let i = ch
                .feature_index(dev, feature::COLOR_LED_EFFECTS)?
                .ok_or_else(|| anyhow!("the headset has no lighting feature"))?
                .0;
            *feat = Some(i);
            i
        }
    };
    for (zone, light) in lighting.zones() {
        let Some(light) = light else { continue };
        let index = effect_index(ch, dev, i, zone, light.effect.id())?;
        let mut params = vec![zone, index];
        params.extend_from_slice(&light.params());
        // Last byte 0: not stored (the zones cannot store anyway).
        params.push(0);
        ch.request(dev, i, 3, &params)?;
    }
    Ok(())
}

/// The index of effect `id` in `zone`'s list, as the headset reports it.
fn effect_index(ch: &mut Channel, dev: u8, feat: u8, zone: u8, id: u16) -> Result<u8> {
    let info = ch.request(dev, feat, 1, &[zone])?;
    let count = *info.get(3).ok_or_else(|| anyhow!("short zone info"))?;
    for e in 0..count {
        let r = ch.request(dev, feat, 2, &[zone, e])?;
        if r.len() >= 4 && u16::from_be_bytes([r[2], r[3]]) == id {
            return Ok(e);
        }
    }
    Err(anyhow!("zone {zone} has no effect {id:04X}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breathing_is_laid_out_as_headsetcontrol_sends_it() {
        // HeadsetControl's G935 "lights on": breathing, 00 b6 ff, 4000 ms,
        // intensity 100.
        let z = ZoneLight {
            effect: Effect::Breathing,
            color: Rgb([0x00, 0xb6, 0xff]),
            period_ms: 4000,
            brightness: 100,
        };
        assert_eq!(z.params(), [0x00, 0xb6, 0xff, 0x0f, 0xa0, 0x00, 0x64, 0, 0, 0]);
    }

    #[test]
    fn cycle_and_steady_put_their_values_where_the_headset_reads_them() {
        let z = ZoneLight {
            effect: Effect::Cycle,
            period_ms: 10_000,
            brightness: 50,
            ..Default::default()
        };
        assert_eq!(z.params(), [0, 0, 0, 0, 0, 0x27, 0x10, 50, 0, 0]);
        let z = ZoneLight { effect: Effect::Static, color: Rgb([1, 2, 3]), ..Default::default() };
        assert_eq!(z.params(), [1, 2, 3, 0, 0, 0, 0, 0, 0, 0]);
        let z = ZoneLight {
            effect: Effect::Static,
            color: Rgb([200, 100, 0]),
            brightness: 50,
            ..Default::default()
        };
        assert_eq!(z.params()[..3], [100, 50, 0]);
    }

    #[test]
    fn out_of_range_values_are_clamped_on_the_way_out() {
        let z = ZoneLight {
            effect: Effect::Breathing,
            period_ms: 0,
            brightness: 0,
            ..Default::default()
        };
        let p = z.params();
        assert_eq!(u16::from_be_bytes([p[3], p[4]]), 1);
        assert_eq!(p[6], 1);
    }

    #[test]
    fn colours_read_and_write_as_hex() {
        assert_eq!("#00B6ff".parse::<Rgb>().unwrap(), Rgb([0x00, 0xb6, 0xff]));
        assert_eq!(Rgb([0x00, 0xb6, 0xff]).to_string(), "#00b6ff");
        assert!("blue".parse::<Rgb>().is_err());
    }

    #[test]
    fn zones_read_from_the_file_and_absent_ones_are_left_alone() {
        #[derive(Deserialize)]
        struct F {
            lighting: HeadsetLighting,
        }
        let f: F =
            toml::from_str("[lighting.sides]\neffect = \"static\"\ncolor = \"#102030\"\n").unwrap();
        let sides = f.lighting.sides.unwrap();
        assert_eq!((sides.effect, sides.color), (Effect::Static, Rgb([0x10, 0x20, 0x30])));
        assert_eq!(f.lighting.logo, None);
        assert!(!HeadsetLighting::default().is_set());
    }
}
