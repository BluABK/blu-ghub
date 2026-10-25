//! `%APPDATA%\BluGHub\BluGHub.toml`.
//!
//! Hand-editable, and every field defaults, so a partial file still loads.
//! A file that fails to parse is reported and the app runs on defaults, but
//! nothing is ever saved over it.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::input::ButtonAction;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    pub headset: Headset,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Headset {
    pub buttons: HeadsetButtons,
}

/// What the G935's G1-G3 do while blu-ghub runs. By default nothing is
/// taken over: the buttons do what the headset's own firmware does with
/// them, and the headset handles its microphone mute by itself.
///
/// Setting any button takes all three over (the headset hands over all or
/// none), and with them the mute button and the boom, which blu-ghub then
/// turns into muting the microphone in Windows. A button left at `default`
/// then does nothing.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HeadsetButtons {
    pub g1: ButtonAction,
    pub g2: ButtonAction,
    pub g3: ButtonAction,
}

impl HeadsetButtons {
    pub fn as_array(&self) -> [ButtonAction; 3] {
        [self.g1, self.g2, self.g3]
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Below this, a device's battery raises a notification (0 = never).
    pub low_battery_percent: u8,
    /// How often a connected device's battery is read when it has not
    /// reported a change by itself, in minutes. Both devices also report on
    /// their own; this is the fallback.
    pub battery_check_minutes: u64,
    pub log_retention_days: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { low_battery_percent: 15, battery_check_minutes: 30, log_retention_days: 7 }
    }
}

impl Config {
    /// Clamp hand-edited values into sane ranges, saying so in the log.
    pub fn sanitized(mut self) -> Config {
        let s = &mut self.settings;
        let clamp = |name: &str, v: u64, lo: u64, hi: u64| -> u64 {
            let c = v.clamp(lo, hi);
            if c != v {
                tracing::warn!("config: {name} = {v} is out of range; using {c}");
            }
            c
        };
        s.low_battery_percent =
            clamp("low_battery_percent", s.low_battery_percent as u64, 0, 100) as u8;
        s.battery_check_minutes = clamp("battery_check_minutes", s.battery_check_minutes, 5, 1440);
        s.log_retention_days = clamp("log_retention_days", s.log_retention_days, 1, 3650);
        self
    }

    /// A missing file is a first run: defaults. An unreadable or invalid one
    /// is an error, so the caller can say so and refuse to overwrite it.
    pub fn load(path: &Path) -> Result<Config> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_the_defaults() {
        let c = Config::load(Path::new(r"C:\does\not\exist\BluGHub.toml")).unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let c: Config = toml::from_str("[settings]\nlow_battery_percent = 25\n").unwrap();
        assert_eq!(c.settings.low_battery_percent, 25);
        assert_eq!(c.settings.battery_check_minutes, 30);
    }

    #[test]
    fn out_of_range_values_are_clamped() {
        let c: Config = toml::from_str(
            "[settings]\nlow_battery_percent = 250\nbattery_check_minutes = 0\nlog_retention_days = 0\n",
        )
        .unwrap();
        let s = c.sanitized().settings;
        assert_eq!(
            (s.low_battery_percent, s.battery_check_minutes, s.log_retention_days),
            (100, 5, 1)
        );
    }

    #[test]
    fn headset_buttons_read_from_the_file() {
        let c: Config =
            toml::from_str("[headset.buttons]\ng1 = \"next_track\"\ng3 = \"disabled\"\n").unwrap();
        assert_eq!(c.headset.buttons.g1, ButtonAction::NextTrack);
        assert_eq!(c.headset.buttons.g2, ButtonAction::Default);
        assert_eq!(c.headset.buttons.g3, ButtonAction::Off);
    }

    #[test]
    fn the_default_config_carries_no_colours_macros_or_overrides() {
        assert_eq!(Config::default().headset.buttons.as_array(), [ButtonAction::Default; 3]);
    }

    #[test]
    fn every_action_name_reads() {
        for name in [
            "default",
            "off",
            "play_pause",
            "stop",
            "next_track",
            "previous_track",
            "mute",
            "volume_up",
            "volume_down",
        ] {
            let text = format!("[headset.buttons]\ng2 = \"{name}\"\n");
            assert!(toml::from_str::<Config>(&text).is_ok(), "{name}");
        }
    }

    #[test]
    fn action_names_are_lower_case_only() {
        assert!(toml::from_str::<Config>("[headset.buttons]\ng2 = \"PlayPause\"\n").is_err());
    }

    #[test]
    fn a_broken_file_is_an_error_not_an_empty_config() {
        assert!(toml::from_str::<Config>("[settings\nlow_battery_percent = ").is_err());
    }
}
