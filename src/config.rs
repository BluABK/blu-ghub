//! `%APPDATA%\BluGHub\BluGHub.toml`.
//!
//! Hand-editable, and every field defaults, so a partial file still loads.
//! A file that fails to parse is reported and the app runs on defaults, but
//! nothing is ever saved over it.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
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
    fn a_broken_file_is_an_error_not_an_empty_config() {
        assert!(toml::from_str::<Config>("[settings\nlow_battery_percent = ").is_err());
    }
}
