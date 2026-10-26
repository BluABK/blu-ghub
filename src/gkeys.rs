//! What the G915's G-keys do while blu-ghub has them, and the macros they
//! can run.
//!
//! ```toml
//! [keyboard.gkeys.m1]
//! g1 = { text = "Hello" }
//! g2 = { media = "play_pause" }
//! g3 = { macro = "greeting" }
//! g4 = "off"
//! # g5 left out: does what the keyboard's own profile says
//!
//! [[macros]]
//! name = "greeting"
//! mode = "once"            # once, while_held, toggle
//! delay_ms = 20            # between steps
//! steps = [{ down = "left_shift" }, { tap = "h" }, { up = "left_shift" }, { text = "i!" }]
//! ```
//!
//! The keyboard hands over all five G-keys or none, so blu-ghub takes them
//! only while some bank gives one an action; keys left at `default` then
//! keep doing what the keyboard's own profile has for them, typed by
//! blu-ghub instead.

use serde::{Deserialize, Serialize};

use crate::input::ButtonAction;
use crate::lighting::Rgb;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GAction {
    /// What the keyboard's own profile has for the key.
    #[default]
    Default,
    /// Nothing.
    #[serde(alias = "disabled", alias = "none")]
    Off,
    /// Type this text, whatever the keyboard layout.
    Text(String),
    /// A media or volume key.
    Media(ButtonAction),
    /// Run the macro of this name.
    Macro(String),
}

impl GAction {
    pub fn is_default(&self) -> bool {
        *self == GAction::Default
    }
}

/// One M bank's G1-G5.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bank {
    #[serde(skip_serializing_if = "GAction::is_default")]
    pub g1: GAction,
    #[serde(skip_serializing_if = "GAction::is_default")]
    pub g2: GAction,
    #[serde(skip_serializing_if = "GAction::is_default")]
    pub g3: GAction,
    #[serde(skip_serializing_if = "GAction::is_default")]
    pub g4: GAction,
    #[serde(skip_serializing_if = "GAction::is_default")]
    pub g5: GAction,
}

impl Bank {
    pub fn keys(&self) -> [&GAction; 5] {
        [&self.g1, &self.g2, &self.g3, &self.g4, &self.g5]
    }

    pub fn keys_mut(&mut self) -> [&mut GAction; 5] {
        [&mut self.g1, &mut self.g2, &mut self.g3, &mut self.g4, &mut self.g5]
    }
}

/// The three banks, as the M1-M3 keys pick them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GKeys {
    /// While blu-ghub has the G-keys (and with them the M keys, whose
    /// lights then go dark), light G1-G5 in the bank's colour.
    pub show_bank: bool,
    /// For M1, M2, M3.
    pub bank_colors: [Rgb; 3],
    /// While blu-ghub has the G-keys, light the bank's M key. The keyboard
    /// only lets the host light them in host mode, so it is switched to
    /// host mode meanwhile, and back to on-board mode when blu-ghub lets go
    /// (a power cycle also ends host mode).
    pub light_m_keys: bool,
    pub m1: Bank,
    pub m2: Bank,
    pub m3: Bank,
}

impl Default for GKeys {
    fn default() -> Self {
        GKeys {
            show_bank: false,
            bank_colors: [
                Rgb([0x20, 0x80, 0xFF]),
                Rgb([0x30, 0xD0, 0x40]),
                Rgb([0xFF, 0x80, 0x10]),
            ],
            light_m_keys: false,
            m1: Bank::default(),
            m2: Bank::default(),
            m3: Bank::default(),
        }
    }
}

impl GKeys {
    pub fn banks(&self) -> [&Bank; 3] {
        [&self.m1, &self.m2, &self.m3]
    }

    pub fn banks_mut(&mut self) -> [&mut Bank; 3] {
        [&mut self.m1, &mut self.m2, &mut self.m3]
    }

    /// Does any key have an action of blu-ghub's? Only then are the
    /// G-keys taken from the keyboard.
    pub fn wanted(&self) -> bool {
        self.banks().iter().any(|b| b.keys().iter().any(|a| !a.is_default()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MacroMode {
    /// Run through once per press.
    #[default]
    Once,
    /// Run over and over while the key is held.
    WhileHeld,
    /// One press starts it running over and over, the next stops it.
    Toggle,
}

impl MacroMode {
    pub const ALL: [MacroMode; 3] = [MacroMode::Once, MacroMode::WhileHeld, MacroMode::Toggle];

    pub fn label(self) -> &'static str {
        match self {
            MacroMode::Once => "Once per press",
            MacroMode::WhileHeld => "Repeat while held",
            MacroMode::Toggle => "Press to start, press to stop",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Press and release a key (a name from the keyboard's keys, or
    /// `f13`-`f24`).
    Tap(String),
    /// Press a key and keep it down (a modifier, say).
    Down(String),
    /// Release a key pressed by `down`. Any still down when the macro ends
    /// are released then.
    Up(String),
    Text(String),
    Media(ButtonAction),
    /// Wait this many milliseconds.
    Delay(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Macro {
    pub name: String,
    pub mode: MacroMode,
    /// Waited after every step that is not a delay.
    pub delay_ms: u32,
    pub steps: Vec<Step>,
}

impl Default for Macro {
    fn default() -> Self {
        Macro { name: String::new(), mode: MacroMode::Once, delay_ms: 20, steps: Vec::new() }
    }
}

impl Macro {
    /// Problems with the macro, for the log: unknown key names, nothing to
    /// do.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.steps.is_empty() {
            out.push(format!("macro {:?} has no steps", self.name));
        }
        for s in &self.steps {
            if let Step::Tap(k) | Step::Down(k) | Step::Up(k) = s
                && crate::inject::key_named(k).is_none()
            {
                out.push(format!("macro {:?}: no key called {k:?}", self.name));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, Deserialize, Serialize, PartialEq)]
    #[serde(default)]
    struct Doc {
        gkeys: GKeys,
        macros: Vec<Macro>,
    }

    #[test]
    fn actions_read_from_the_file() {
        let d: Doc = toml::from_str(
            "[gkeys.m2]\ng1 = { text = \"hi\" }\ng2 = { media = \"mute\" }\n\
             g3 = { macro = \"m\" }\ng4 = \"off\"\n",
        )
        .unwrap();
        let b = &d.gkeys.m2;
        assert_eq!(b.g1, GAction::Text("hi".into()));
        assert_eq!(b.g2, GAction::Media(ButtonAction::Mute));
        assert_eq!(b.g3, GAction::Macro("m".into()));
        assert_eq!(b.g4, GAction::Off);
        assert_eq!(b.g5, GAction::Default);
        assert!(d.gkeys.wanted());
    }

    #[test]
    fn the_bank_colours_read_and_default() {
        let d: Doc = toml::from_str(
            "[gkeys]\nshow_bank = true\nbank_colors = [\"#010203\", \"#040506\", \"#070809\"]\n",
        )
        .unwrap();
        assert!(d.gkeys.show_bank);
        assert_eq!(d.gkeys.bank_colors[2], Rgb([7, 8, 9]));
        assert!(!GKeys::default().show_bank);
        assert!(!GKeys::default().light_m_keys);
        // Showing the bank alone takes nothing over.
        assert!(!d.gkeys.wanted());
    }

    #[test]
    fn a_bank_without_overrides_is_never_diverted() {
        assert!(!GKeys::default().wanted());
        let d: Doc = toml::from_str("[gkeys.m1]\ng1 = \"default\"\n").unwrap();
        assert!(!d.gkeys.wanted());
    }

    #[test]
    fn macros_read_and_write_back() {
        let text = "[[macros]]\nname = \"m\"\nmode = \"while_held\"\nsteps = [\
                    { down = \"left_shift\" }, { tap = \"a\" }, { delay = 50 }, \
                    { up = \"left_shift\" }, { media = \"next_track\" }]\n";
        let d: Doc = toml::from_str(text).unwrap();
        let m = &d.macros[0];
        assert_eq!(m.mode, MacroMode::WhileHeld);
        assert_eq!(m.delay_ms, 20);
        assert_eq!(m.steps[2], Step::Delay(50));
        assert!(m.problems().is_empty());
        let again: Doc = toml::from_str(&toml::to_string(&d).unwrap()).unwrap();
        assert_eq!(again, d);
    }

    #[test]
    fn unknown_keys_are_reported() {
        let m =
            Macro { name: "x".into(), steps: vec![Step::Tap("nope".into())], ..Default::default() };
        assert_eq!(m.problems().len(), 1);
    }

    #[test]
    fn unset_keys_are_not_written() {
        let mut d = Doc::default();
        d.gkeys.m1.g3 = GAction::Off;
        let text = toml::to_string(&d).unwrap();
        assert!(text.contains("g3 = \"off\""));
        assert!(!text.contains("g1"));
    }
}
