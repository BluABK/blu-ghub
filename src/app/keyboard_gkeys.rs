//! The G915's G-keys while blu-ghub has them.
//!
//! The keyboard hands over all five G-keys or none (0x8010 fn2), so they are
//! taken only while some bank gives a key an action of blu-ghub's. A key left
//! at `default` then still does what the keyboard's own profile for that
//! bank says: the bindings of all three profiles are read (0x8100 reads)
//! when the keys are taken, and typed by blu-ghub.
//!
//! Taking the G-keys takes the M keys with them (verified): M1-M3 then no
//! longer switch the keyboard's profile, they report to blu-ghub (0x8020
//! events, bit 0 = M1), and their lights go out. So blu-ghub keeps the bank
//! itself: it starts at the keyboard's profile when the keys are taken
//! (0x8100 fn4, a read) and follows the M keys from there. The keyboard
//! stays on its profile throughout, and is back on it, lights and all, when
//! the keys are handed back. The keyboard does not show M lights asked for
//! by the host in on-board mode (0x8020 fn1 is accepted, nothing lights),
//! so blu-ghub can light G1-G5 in the bank's colour instead
//! ([`Takeover::indicator`], painted by the keyboard worker's lighting), or,
//! with `light_m_keys`, switch the keyboard to host mode (0x8100 fn1
//! `[02]`) while it has the keys, where the M lights do follow 0x8020 fn1
//! (verified). Host mode turns the on-board profiles off, which blu-ghub
//! stands in for while it has the keys; it switches back to on-board mode
//! (`[01]`) when it lets go, and a power cycle ends host mode too
//! (verified). A keyboard found in host mode while not wanted is switched
//! back at the first contact.
//!
//! The takeover ends when the keyboard sleeps or goes away (it is taken
//! again when it wakes), and blu-ghub hands the keys back when it quits, or
//! on a panic (see [`emergency_hand_back`]).

use std::collections::HashMap;
use std::sync::Mutex;

use tracing::{debug, info, warn};

use crate::gkeys::{GAction, GKeys, Macro};
use crate::hidpp::{self, Channel, HidppError, feature};
use crate::inject;
use crate::input::ButtonAction;
use crate::lighting::Rgb;
use crate::macros::{Action, Cmd, Runner};

/// What the keyboard worker needs to know.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Setup {
    pub gkeys: GKeys,
    pub macros: Vec<Macro>,
}

/// While the keys are taken, for [`emergency_hand_back`]: the keyboard's
/// channel handle, device index and GKEY feature, and ONBOARD_PROFILES if
/// the keyboard is in host mode.
#[derive(Clone, Copy)]
struct Taken {
    raw: isize,
    dev: u8,
    gkey: u8,
    host: Option<u8>,
}

static TAKEN: Mutex<Option<Taken>> = Mutex::new(None);

fn taken() -> std::sync::MutexGuard<'static, Option<Taken>> {
    TAKEN.lock().unwrap_or_else(|e| e.into_inner())
}

/// For the panic hook: the G-keys back, and on-board mode if blu-ghub
/// switched it off. Writes only, no reply awaited, so the G-keys do not
/// stay dead until the keyboard is switched off.
pub fn emergency_hand_back() {
    let Ok(guard) = TAKEN.try_lock() else { return };
    if let Some(t) = *guard {
        // Borrowed: the channel that owns the handle closes it.
        let handle = std::mem::ManuallyDrop::new(crate::hid::OwnedHandle(
            windows::Win32::Foundation::HANDLE(t.raw as *mut _),
        ));
        let frame = [hidpp::LONG, t.dev, t.gkey, 0x20 | hidpp::SW_ID, 0x00];
        let _ = crate::hid::write_report(&handle, &frame, hidpp::LONG_LEN);
        if let Some(o) = t.host {
            let frame = [hidpp::LONG, t.dev, o, 0x10 | hidpp::SW_ID, ONBOARD_MODE];
            let _ = crate::hid::write_report(&handle, &frame, hidpp::LONG_LEN);
        }
    }
}

/// 0x8100 fn1/fn2 modes.
const ONBOARD_MODE: u8 = 0x01;
const HOST_MODE: u8 = 0x02;

/// After switching back to on-board mode, the keyboard is given this long
/// before the G-keys are taken again: taken at once, its own profile also
/// answered the first press (seen once: a G-key typed its on-board key
/// over blu-ghub's text).
const MODE_SETTLE: std::time::Duration = std::time::Duration::from_millis(250);

/// Where a profile sector keeps G1-G5's bindings, 4 bytes each.
const BUTTONS_AT: u16 = 32;

pub struct Takeover {
    setup: Setup,
    gkey: Option<u8>,
    onboard: Option<u8>,
    /// MKEYS: the M keys' reports while the keys are taken; looked up
    /// when they are.
    mkeys: Option<u8>,
    diverted: bool,
    /// A takeover was asked for, so a hand-back is owed at the end.
    attempted: bool,
    failed: bool,
    /// The one hand-back at the first contact while nothing is wanted,
    /// which undoes a takeover left by a killed run.
    cleared: bool,
    /// The mode was checked at the first contact (a killed run may have
    /// left the keyboard in host mode).
    mode_checked: bool,
    /// blu-ghub switched the keyboard to host mode (and has not switched
    /// it back).
    host: bool,
    /// Said once that G HUB has the keys.
    ghub_said: bool,
    /// G-keys down, bit 0 = G1.
    mask: u8,
    /// Profile sectors in M1-M3 order, and their G1-G5 bindings.
    banks: Vec<u16>,
    bindings: HashMap<u16, [[u8; 4]; 5]>,
    bank: usize,
    /// Bindings blu-ghub cannot type, said once each.
    unsupported_said: Vec<[u8; 4]>,
    runner: Option<Runner>,
}

impl Takeover {
    pub fn new(setup: Setup) -> Self {
        for m in &setup.macros {
            for p in m.problems() {
                warn!("G915: {p}");
            }
        }
        for p in setup.gkeys.problems() {
            warn!("G915: {p}");
        }
        Takeover {
            setup,
            gkey: None,
            onboard: None,
            mkeys: None,
            diverted: false,
            attempted: false,
            failed: false,
            cleared: false,
            mode_checked: false,
            host: false,
            ghub_said: false,
            mask: 0,
            banks: Vec::new(),
            bindings: HashMap::new(),
            bank: 0,
            unsupported_said: Vec::new(),
            runner: None,
        }
    }

    /// The feature index G-key events arrive on, once known.
    pub fn event_feature(&self) -> Option<u8> {
        self.gkey
    }

    /// blu-ghub's bank, while it has the keys (and with them the M keys).
    pub fn bank_label(&self) -> Option<String> {
        self.diverted.then(|| format!("M{} (blu-ghub's bank)", self.bank + 1))
    }

    /// The feature index M-key events arrive on while the keys are taken.
    pub fn mkey_feature(&self) -> Option<u8> {
        self.mkeys
    }

    /// An M-key report while blu-ghub has the keys: the key pressed picks
    /// the bank (the release, all bits clear, is ignored). `true` if the
    /// bank changed.
    pub fn on_mkeys(&mut self, ch: &mut Channel, dev: u8, mask: u8) -> bool {
        if !self.diverted || mask == 0 {
            return false;
        }
        let bank = mask.trailing_zeros() as usize;
        if bank >= 3 || bank == self.bank {
            return false;
        }
        debug!("G915: bank M{}", bank + 1);
        self.bank = bank;
        self.light(ch, dev);
        true
    }

    /// G1-G5 in the bank's colour, while blu-ghub has the keys and the
    /// settings ask for it.
    pub fn indicator(&self) -> Option<Vec<(u8, Rgb)>> {
        let g = &self.setup.gkeys;
        (self.diverted && g.show_bank).then(|| {
            let colour = g.bank_colors[self.bank.min(2)];
            ["g1", "g2", "g3", "g4", "g5"]
                .iter()
                .filter_map(|n| crate::keymap::by_name(n))
                .map(|k| (k.zone, colour))
                .collect()
        })
    }

    fn features(&mut self, ch: &mut Channel, dev: u8) -> anyhow::Result<(u8, u8)> {
        let find = |ch: &mut Channel, id: u16, name: &str| -> anyhow::Result<u8> {
            match ch.feature_index(dev, id)? {
                Some((i, _, _)) => Ok(i),
                None => anyhow::bail!("the keyboard has no {name} feature"),
            }
        };
        let g = match self.gkey {
            Some(g) => g,
            None => *self.gkey.insert(find(ch, feature::GKEY, "GKEY")?),
        };
        let o = match self.onboard {
            Some(o) => o,
            None => *self.onboard.insert(find(ch, feature::ONBOARD_PROFILES, "ONBOARD_PROFILES")?),
        };
        Ok((g, o))
    }

    fn host_wanted(&self) -> bool {
        self.setup.gkeys.wanted() && self.setup.gkeys.light_m_keys
    }

    /// The keyboard is awake: take the keys if wanted and not yet taken.
    pub fn settle(&mut self, ch: &mut Channel, dev: u8) {
        if !self.mode_checked && !crate::platform::ghub_running() {
            self.mode_checked = true;
            self.repair_mode(ch, dev);
        }
        if !self.setup.gkeys.wanted() {
            if !self.cleared {
                self.cleared = true;
                if !crate::platform::ghub_running() {
                    match self.features(ch, dev).and_then(|(g, _)| {
                        ch.request(dev, g, 2, &[0x00])?;
                        Ok(())
                    }) {
                        Ok(()) => debug!("G915: the G-keys are the keyboard's"),
                        Err(e) => debug!("G915: could not make sure of the G-keys: {e:#}"),
                    }
                }
            }
            return;
        }
        if self.diverted || self.failed {
            return;
        }
        if crate::platform::ghub_running() {
            if !self.ghub_said {
                self.ghub_said = true;
                info!("G915: G HUB is running; leaving the G-keys to it");
            }
            return;
        }
        match self.take(ch, dev) {
            Ok(()) => {}
            Err(e) => {
                warn!("G915: could not take the G-keys over: {e:#}");
                self.failed = true;
            }
        }
    }

    fn take(&mut self, ch: &mut Channel, dev: u8) -> anyhow::Result<()> {
        let (g, o) = self.features(ch, dev)?;
        if self.banks.is_empty() {
            self.read_bindings(ch, dev, o)?;
        }
        self.attempted = true;
        ch.request(dev, g, 2, &[0x01])?;
        self.diverted = true;
        self.mask = 0;
        *taken() = Some(Taken { raw: ch.raw_long_handle(), dev, gkey: g, host: None });
        if self.runner.is_none() {
            self.runner = Some(Runner::start());
        }
        info!("G915: G-keys taken over");
        if self.mkeys.is_none() {
            self.mkeys = ch.feature_index(dev, feature::MKEYS).ok().flatten().map(|f| f.0);
        }
        // Before host mode: the profile in use is the bank to start from.
        self.bank_now(ch, dev);
        if self.setup.gkeys.light_m_keys {
            self.enter_host(ch, dev);
        }
        Ok(())
    }

    /// Host mode, so the M lights can be lit, and the bank's lit.
    fn enter_host(&mut self, ch: &mut Channel, dev: u8) {
        let (Some(g), Some(o)) = (self.gkey, self.onboard) else { return };
        match ch.request(dev, o, 1, &[HOST_MODE]) {
            Ok(_) => {
                self.host = true;
                *taken() = Some(Taken { raw: ch.raw_long_handle(), dev, gkey: g, host: Some(o) });
                info!("G915: host mode, for the M lights");
                self.light(ch, dev);
            }
            Err(e) => warn!("G915: could not switch to host mode for the M lights: {e}"),
        }
    }

    /// Back to on-board mode, if blu-ghub left it.
    fn leave_host(&mut self, ch: &mut Channel, dev: u8) {
        let (true, Some(o)) = (self.host, self.onboard) else { return };
        match ch.request(dev, o, 1, &[ONBOARD_MODE]) {
            Ok(_) => info!("G915: on-board mode again"),
            Err(e) => warn!(
                "G915: could not switch back to on-board mode ({e}); switching the keyboard \
                 off and on does"
            ),
        }
        self.host = false;
        if let Some(t) = taken().as_mut() {
            t.host = None;
        }
    }

    /// At the first contact: a keyboard left in host mode (by a run that
    /// was killed, say) goes back to on-board mode unless host mode is
    /// wanted now. Reads the mode; writes only to repair it.
    fn repair_mode(&mut self, ch: &mut Channel, dev: u8) {
        let Ok((_, o)) = self.features(ch, dev) else { return };
        match ch.request(dev, o, 2, &[]) {
            Ok(p) if p.first() == Some(&HOST_MODE) && !self.host_wanted() => {
                match ch.request(dev, o, 1, &[ONBOARD_MODE]) {
                    Ok(_) => {
                        info!("G915: the keyboard was left in host mode; back to on-board");
                        std::thread::sleep(MODE_SETTLE);
                    }
                    Err(e) => warn!("G915: the keyboard is in host mode and stays there: {e}"),
                }
            }
            Ok(_) => {}
            Err(e) => debug!("G915: mode: {e}"),
        }
    }

    /// In host mode: light the bank's M key (bit 0 = M1).
    fn light(&mut self, ch: &mut Channel, dev: u8) {
        if !self.host {
            return;
        }
        let Some(m) = self.mkeys else { return };
        if let Err(e) = ch.request(dev, m, 1, &[1 << self.bank.min(7)]) {
            debug!("G915: M light: {e}");
        }
    }

    /// Ask the keyboard which profile it is on: blu-ghub's bank starts
    /// there.
    fn bank_now(&mut self, ch: &mut Channel, dev: u8) {
        let Some(o) = self.onboard else { return };
        match ch.request(dev, o, 4, &[]) {
            Ok(p) if p.len() >= 2 => {
                let sector = u16::from_be_bytes([p[0], p[1]]);
                if let Some(b) = self.banks.iter().position(|s| *s == sector) {
                    self.bank = b;
                }
            }
            Ok(_) => {}
            Err(e) => debug!("G915: which bank: {e}; taking M{}", self.bank + 1),
        }
    }

    /// The profiles' sectors from the directory (sector 0), and each one's
    /// G1-G5 bindings. Reads only.
    fn read_bindings(&mut self, ch: &mut Channel, dev: u8, o: u8) -> anyhow::Result<()> {
        let read = |ch: &mut Channel, sector: u16, at: u16| -> Result<Vec<u8>, HidppError> {
            ch.request(dev, o, 5, &[(sector >> 8) as u8, sector as u8, (at >> 8) as u8, at as u8])
        };
        let dir = read(ch, 0, 0)?;
        self.banks = directory(&dir);
        for &s in &self.banks {
            let mut bytes = read(ch, s, BUTTONS_AT)?;
            bytes.extend(read(ch, s, BUTTONS_AT + 16)?);
            let mut keys = [[0xFF; 4]; 5];
            for (i, k) in keys.iter_mut().enumerate() {
                k.copy_from_slice(&bytes[i * 4..i * 4 + 4]);
            }
            self.bindings.insert(s, keys);
        }
        debug!("G915: profiles in sectors {:?}", self.banks);
        Ok(())
    }

    /// A G-key report: which keys are down now.
    pub fn on_keys(&mut self, ch: &mut Channel, dev: u8, mask: u8) {
        if !self.diverted {
            // Taken after all (a reply that timed out), or by a run that
            // was killed: either way blu-ghub has them now.
            if !self.setup.gkeys.wanted() {
                return;
            }
            info!("G915: the G-keys were taken over after all");
            self.diverted = true;
            self.attempted = true;
            if let Some(g) = self.gkey {
                *taken() = Some(Taken { raw: ch.raw_long_handle(), dev, gkey: g, host: None });
            }
            if self.runner.is_none() {
                self.runner = Some(Runner::start());
            }
        }
        let pressed = mask & !self.mask;
        let released = self.mask & !mask;
        self.mask = mask;
        let Some(runner) = &self.runner else { return };
        for i in 0..5u8 {
            if released & (1 << i) != 0 {
                runner.send(Cmd::Release { key: i });
            }
        }
        if pressed & 0x1F == 0 {
            return;
        }
        for i in 0..5u8 {
            if pressed & (1 << i) == 0 {
                continue;
            }
            let action = self.action(i as usize);
            debug!("G915: G{} in M{}: {}", i + 1, self.bank + 1, summary(action.as_ref()));
            if let Some(action) = action
                && let Some(runner) = &self.runner
            {
                runner.send(Cmd::Press { key: i, action });
            }
        }
    }

    fn action(&mut self, key: usize) -> Option<Action> {
        let bank = self.setup.gkeys.banks()[self.bank.min(2)];
        match bank.keys()[key] {
            GAction::Default => {
                let sector = *self.banks.get(self.bank)?;
                let b = self.bindings.get(&sector)?[key];
                let action = binding_action(b);
                if action.is_none() && b != [0xFF; 4] && !self.unsupported_said.contains(&b) {
                    self.unsupported_said.push(b);
                    info!(
                        "G915: G{} in M{} is {} on the keyboard, which blu-ghub cannot do while \
                         it has the G-keys",
                        key + 1,
                        self.bank + 1,
                        crate::onboard::Binding::describe(&b)
                    );
                }
                action
            }
            GAction::Off => None,
            GAction::Key(s) => {
                let keys = crate::gkeys::Combo::parse(s).keys();
                if keys.is_none() {
                    warn!("G915: G{} in M{}: no key {s:?}", key + 1, self.bank + 1);
                }
                keys.map(Action::Keys)
            }
            GAction::Text(t) => Some(Action::Text(t.clone())),
            GAction::Paste(t) => Some(Action::Paste(t.clone())),
            GAction::PasteFile(p) => Some(Action::PasteFile(p.clone())),
            GAction::Media(a) => match a {
                ButtonAction::Default | ButtonAction::Off => None,
                a => Some(Action::Media(*a)),
            },
            GAction::Macro(name) => {
                let m = self.setup.macros.iter().find(|m| m.name == *name);
                if m.is_none() {
                    warn!("G915: no macro called {name:?}");
                }
                m.cloned().map(Action::Macro)
            }
        }
    }

    /// The keyboard slept or went away, which ends the takeover; it is
    /// taken again when the keyboard is back.
    pub fn forget(&mut self) {
        if let Some(r) = &self.runner {
            r.send(Cmd::ReleaseAll);
        }
        // `host` stays: whether sleep ends host mode is not known, so the
        // switch back is still tried at the end.
        self.diverted = false;
        self.mask = 0;
        *taken() = None;
    }

    /// Take the keys again even if blu-ghub believes it has them (after a
    /// refresh or resume): taking over twice does no harm.
    pub fn invalidate(&mut self) {
        self.diverted = false;
        self.failed = false;
    }

    /// On quit: let go of everything and hand the keys back.
    pub fn release(&mut self, ch: &mut Channel, dev: u8) {
        self.runner = None;
        if self.attempted || self.diverted || self.host {
            self.let_go(ch, dev);
        }
        *taken() = None;
    }

    /// Hand the G-keys back, and on-board mode if blu-ghub left it.
    fn let_go(&mut self, ch: &mut Channel, dev: u8) {
        if let Some(r) = &self.runner {
            r.send(Cmd::ReleaseAll);
        }
        if crate::platform::ghub_running() {
            info!("G915: G HUB is running; leaving the G-keys to it");
        } else if let Some(g) = self.gkey {
            match ch.request(dev, g, 2, &[0x00]) {
                Ok(_) => info!("G915: G-keys handed back"),
                Err(e) => warn!("G915: could not hand the G-keys back: {e}"),
            }
        }
        self.diverted = false;
        self.attempted = false;
        self.mask = 0;
        // On-board mode back whatever runs: it is the keyboard's normal.
        self.leave_host(ch, dev);
        *taken() = None;
    }

    /// New settings while running: send only what they change. Different
    /// actions or macros send nothing; keys no longer wanted are handed
    /// back, newly wanted ones taken, and host mode follows `light_m_keys`.
    /// While the keyboard sleeps nothing is sent; its wake sorts it out.
    ///
    /// `true` when the keyboard's mode changed: that wipes its lighting
    /// (the claim and painted keys; seen on this keyboard), so the caller
    /// paints afresh.
    pub fn update(&mut self, ch: &mut Channel, dev: u8, setup: Setup, connected: bool) -> bool {
        for m in &setup.macros {
            for p in m.problems() {
                warn!("G915: {p}");
            }
        }
        self.setup = setup;
        // A held key's action may have changed under it.
        if let Some(r) = &self.runner {
            r.send(Cmd::ReleaseAll);
        }
        self.mask = 0;
        self.failed = false;
        self.unsupported_said.clear();
        let wanted = self.setup.gkeys.wanted();
        if !connected {
            if !wanted {
                self.cleared = false;
                self.mode_checked = false;
            }
            return false;
        }
        let was_host = self.host;
        if self.diverted && !wanted {
            self.let_go(ch, dev);
            self.cleared = true;
        } else if self.diverted && self.setup.gkeys.light_m_keys != self.host {
            // Switching the mode under the taken keys leaves the keyboard's
            // M lights and lighting in a muddle (seen: its own profile's M
            // lit, or everything dark), so the whole takeover is redone,
            // keeping the bank.
            self.retake(ch, dev);
        } else if !self.diverted {
            self.settle(ch, dev);
        }
        was_host != self.host
    }

    /// Let go and take the keys again, in the bank blu-ghub was in.
    fn retake(&mut self, ch: &mut Channel, dev: u8) {
        let bank = self.bank;
        let was_host = self.host;
        self.let_go(ch, dev);
        if was_host {
            std::thread::sleep(MODE_SETTLE);
        }
        if let Err(e) = self.take(ch, dev) {
            warn!("G915: could not take the G-keys over again: {e:#}");
            self.failed = true;
            return;
        }
        if self.bank != bank {
            self.bank = bank;
            self.light(ch, dev);
        }
    }
}

/// For the log: what a press does, without the text it types.
fn summary(a: Option<&Action>) -> String {
    match a {
        None => "nothing".into(),
        Some(Action::Keys(k)) => format!("{} key(s) held", k.len()),
        Some(Action::Text(t)) => format!("text, {} characters", t.chars().count()),
        Some(Action::Paste(t)) => format!("paste, {} characters", t.chars().count()),
        Some(Action::PasteFile(_)) => "paste a file".into(),
        Some(Action::Media(m)) => format!("{m:?}"),
        Some(Action::Macro(m)) => format!("macro {:?}", m.name),
    }
}

/// The enabled profiles' sectors in a directory sector: 4-byte entries
/// `[sector hi, sector lo, enabled, ...]`, ended by `FF FF`.
fn directory(dir: &[u8]) -> Vec<u16> {
    dir.chunks(4)
        .take_while(|e| e.len() == 4 && !(e[0] == 0xFF && e[1] == 0xFF))
        .filter(|e| e[2] != 0)
        .map(|e| u16::from_be_bytes([e[0], e[1]]))
        .collect()
}

/// What blu-ghub types for an on-board binding, if it can.
fn binding_action(b: [u8; 4]) -> Option<Action> {
    match b {
        [0x80, 0x02, mods, usage] => {
            let mut keys = inject::modifiers(mods);
            if usage != 0 {
                keys.push(inject::from_usage(usage)?);
            }
            (!keys.is_empty()).then_some(Action::Keys(keys))
        }
        [0x80, 0x03, hi, lo] => {
            let media = match u16::from_be_bytes([hi, lo]) {
                0xCD => ButtonAction::PlayPause,
                0xB7 => ButtonAction::Stop,
                0xB5 => ButtonAction::NextTrack,
                0xB6 => ButtonAction::PreviousTrack,
                0xE2 => ButtonAction::Mute,
                0xE9 => ButtonAction::VolumeUp,
                0xEA => ButtonAction::VolumeDown,
                _ => return None,
            };
            Some(Action::Media(media))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_lists_enabled_profiles_in_order() {
        let dir = [0, 1, 1, 0, 0, 2, 1, 0, 0, 3, 0, 0, 0, 4, 1, 0, 0xFF, 0xFF, 0, 0];
        assert_eq!(directory(&dir), [1, 2, 4]);
    }

    #[test]
    fn on_board_keys_are_typed_with_their_modifiers() {
        let a = binding_action([0x80, 0x02, 0x01, 0x6A]).unwrap();
        assert_eq!(
            a,
            Action::Keys(vec![
                inject::key_named("left_ctrl").unwrap(),
                inject::key_named("f15").unwrap()
            ])
        );
        // A modifier on its own.
        assert_eq!(
            binding_action([0x80, 0x02, 0x01, 0x00]),
            Some(Action::Keys(vec![inject::key_named("left_ctrl").unwrap()]))
        );
        assert_eq!(
            binding_action([0x80, 0x03, 0x00, 0xCD]),
            Some(Action::Media(ButtonAction::PlayPause))
        );
    }

    #[test]
    fn what_cannot_be_typed_is_left_alone() {
        assert_eq!(binding_action([0xFF; 4]), None);
        assert_eq!(binding_action([0x90, 0x0D, 0x00, 0x02]), None);
        assert_eq!(binding_action([0x00, 0x01, 0x00, 0x10]), None);
    }
}
