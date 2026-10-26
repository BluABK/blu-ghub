//! Carrying out G-key actions, on a thread of its own so that a macro's
//! delays never hold up the keyboard's worker.
//!
//! The worker sends a [`Cmd`] per press and release. Keys typed for a held
//! G-key stay down until it is released; a macro runs once, over and over
//! while its key is held, or from one press to the next. One macro runs at
//! a time: a macro key pressed meanwhile is ignored, other keys still work.
//! Whatever is down when the keyboard goes away, or blu-ghub lets the keys
//! go, is released: nothing is ever left held.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use crate::gkeys::{Macro, MacroMode, Step};
use crate::inject::{self, Key};
use crate::input::ButtonAction;

/// A repeating macro runs at most this often, however short its steps.
const MIN_ROUND: Duration = Duration::from_millis(20);

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Keys held for as long as the G-key is (modifiers first).
    Keys(Vec<Key>),
    Text(String),
    Media(ButtonAction),
    Macro(Macro),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Press {
        key: u8,
        action: Action,
    },
    Release {
        key: u8,
    },
    /// Release everything and stop any macro.
    ReleaseAll,
    Quit,
}

/// Where the typing goes: Windows, or a recording in tests.
pub trait Out {
    fn down(&mut self, k: Key);
    fn up(&mut self, k: Key);
    fn text(&mut self, t: &str);
    fn media(&mut self, a: ButtonAction);
}

pub struct Windows;

impl Out for Windows {
    fn down(&mut self, k: Key) {
        if let Err(e) = inject::down(k) {
            debug!("typing: {e}");
        }
    }
    fn up(&mut self, k: Key) {
        if let Err(e) = inject::up(k) {
            debug!("typing: {e}");
        }
    }
    fn text(&mut self, t: &str) {
        if let Err(e) = inject::text(t) {
            debug!("typing: {e}");
        }
    }
    fn media(&mut self, a: ButtonAction) {
        if let Err(e) = a.run() {
            debug!("typing: {e}");
        }
    }
}

/// The running thread.
pub struct Runner {
    tx: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
}

impl Runner {
    pub fn start() -> Runner {
        let (tx, rx) = channel();
        let thread = std::thread::Builder::new()
            .name("gkeys".into())
            .spawn(move || Engine::new(Windows, rx).run())
            .map_err(|e| warn!("cannot start the G-key thread: {e}"))
            .ok();
        Runner { tx, thread }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Why a macro stopped early.
enum Stop {
    /// Its key was released (while-held) or pressed again (toggle).
    Done,
    /// Everything is to be let go.
    ReleaseAll,
    Quit,
}

pub struct Engine<O: Out> {
    out: O,
    rx: Receiver<Cmd>,
    /// Keys typed for held G-keys.
    held: HashMap<u8, Vec<Key>>,
}

impl<O: Out> Engine<O> {
    pub fn new(out: O, rx: Receiver<Cmd>) -> Self {
        Engine { out, rx, held: HashMap::new() }
    }

    pub fn run(mut self) {
        while let Ok(cmd) = self.rx.recv() {
            if !self.handle(cmd) {
                break;
            }
        }
        self.release_all();
    }

    #[cfg(test)]
    fn into_out(self) -> O {
        self.out
    }

    /// `false`: quit.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Press { key, action: Action::Macro(m) } => match self.run_macro(key, &m) {
                Some(Stop::Quit) => return false,
                Some(Stop::ReleaseAll) => self.release_all(),
                _ => {}
            },
            Cmd::Press { key, action } => self.simple(key, action),
            Cmd::Release { key } => self.release(key),
            Cmd::ReleaseAll => self.release_all(),
            Cmd::Quit => return false,
        }
        true
    }

    /// Anything but a macro: done at once, even while a macro runs.
    fn simple(&mut self, key: u8, action: Action) {
        match action {
            Action::Keys(keys) => {
                self.release(key);
                for k in &keys {
                    self.out.down(*k);
                }
                self.held.insert(key, keys);
            }
            Action::Text(t) => self.out.text(&t),
            Action::Media(a) => self.out.media(a),
            Action::Macro(m) => debug!("macro {:?}: another one is running", m.name),
        }
    }

    fn release(&mut self, key: u8) {
        if let Some(keys) = self.held.remove(&key) {
            for k in keys.iter().rev() {
                self.out.up(*k);
            }
        }
    }

    fn release_all(&mut self) {
        let keys: Vec<u8> = self.held.keys().copied().collect();
        for k in keys {
            self.release(k);
        }
    }

    fn run_macro(&mut self, key: u8, m: &Macro) -> Option<Stop> {
        debug!("macro {:?} ({:?})", m.name, m.mode);
        let mut down: Vec<Key> = Vec::new();
        let stop = 'rounds: loop {
            let started = Instant::now();
            for step in &m.steps {
                let wait = match step {
                    Step::Delay(ms) => Duration::from_millis(*ms as u64),
                    other => {
                        self.step(other, &mut down);
                        Duration::from_millis(m.delay_ms as u64)
                    }
                };
                if let Some(s) = self.wait(key, m.mode, wait) {
                    break 'rounds Some(s);
                }
            }
            if m.mode == MacroMode::Once {
                break None;
            }
            let rest = MIN_ROUND.saturating_sub(started.elapsed());
            if let Some(s) = self.wait(key, m.mode, rest) {
                break Some(s);
            }
        };
        for k in down.iter().rev() {
            self.out.up(*k);
        }
        stop
    }

    fn step(&mut self, step: &Step, down: &mut Vec<Key>) {
        match step {
            Step::Tap(k) => {
                if let Some(k) = inject::key_named(k) {
                    self.out.down(k);
                    self.out.up(k);
                }
            }
            Step::Down(k) => {
                if let Some(k) = inject::key_named(k)
                    && !down.contains(&k)
                {
                    self.out.down(k);
                    down.push(k);
                }
            }
            Step::Up(k) => {
                if let Some(k) = inject::key_named(k)
                    && let Some(i) = down.iter().position(|d| *d == k)
                {
                    self.out.up(k);
                    down.remove(i);
                }
            }
            Step::Text(t) => self.out.text(t),
            Step::Media(a) => self.out.media(*a),
            Step::Delay(_) => {}
        }
    }

    /// Wait `d`, doing whatever else comes in meanwhile; `Some` when the
    /// macro has to stop.
    fn wait(&mut self, key: u8, mode: MacroMode, d: Duration) -> Option<Stop> {
        let until = Instant::now() + d;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            let cmd = if left.is_zero() {
                match self.rx.try_recv() {
                    Ok(c) => c,
                    Err(_) => return None,
                }
            } else {
                match self.rx.recv_timeout(left) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => return None,
                    Err(RecvTimeoutError::Disconnected) => return Some(Stop::Quit),
                }
            };
            match cmd {
                Cmd::Quit => return Some(Stop::Quit),
                Cmd::ReleaseAll => return Some(Stop::ReleaseAll),
                Cmd::Release { key: k } if k == key => {
                    if mode == MacroMode::WhileHeld {
                        return Some(Stop::Done);
                    }
                }
                Cmd::Press { key: k, .. } if k == key => {
                    if mode == MacroMode::Toggle {
                        return Some(Stop::Done);
                    }
                }
                Cmd::Press { key: k, action } => self.simple(k, action),
                Cmd::Release { key: k } => self.release(k),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Record(Vec<String>);

    impl Out for Record {
        fn down(&mut self, k: Key) {
            self.0.push(format!("down {k:?}"));
        }
        fn up(&mut self, k: Key) {
            self.0.push(format!("up {k:?}"));
        }
        fn text(&mut self, t: &str) {
            self.0.push(format!("text {t}"));
        }
        fn media(&mut self, a: ButtonAction) {
            self.0.push(format!("media {a:?}"));
        }
    }

    fn key(name: &str) -> Key {
        inject::key_named(name).unwrap()
    }

    /// Run the commands through an engine and return what it typed.
    fn run(cmds: Vec<Cmd>) -> Vec<String> {
        let (tx, rx) = channel();
        for c in cmds {
            tx.send(c).unwrap();
        }
        drop(tx);
        let mut e = Engine::new(Record::default(), rx);
        while let Ok(c) = e.rx.recv() {
            if !e.handle(c) {
                break;
            }
        }
        e.release_all();
        e.into_out().0
    }

    fn mac(mode: MacroMode, steps: Vec<Step>) -> Macro {
        Macro { name: "t".into(), mode, delay_ms: 0, steps }
    }

    #[test]
    fn held_keys_go_up_in_reverse_on_release() {
        let typed = run(vec![
            Cmd::Press { key: 0, action: Action::Keys(vec![key("left_ctrl"), key("c")]) },
            Cmd::Release { key: 0 },
        ]);
        assert_eq!(typed.len(), 4);
        assert!(typed[2].contains("code: 46") && typed[2].starts_with("up"), "{typed:?}");
        assert!(typed[3].contains("code: 29") && typed[3].starts_with("up"), "{typed:?}");
    }

    #[test]
    fn link_loss_releases_every_injected_key() {
        let typed = run(vec![
            Cmd::Press { key: 0, action: Action::Keys(vec![key("left_shift")]) },
            Cmd::Press { key: 1, action: Action::Keys(vec![key("f15")]) },
            Cmd::ReleaseAll,
        ]);
        assert_eq!(typed.iter().filter(|t| t.starts_with("down")).count(), 2);
        assert_eq!(typed.iter().filter(|t| t.starts_with("up")).count(), 2);
    }

    #[test]
    fn a_macro_releases_what_it_left_down() {
        let m = mac(
            MacroMode::Once,
            vec![Step::Down("left_shift".into()), Step::Tap("a".into()), Step::Text("é".into())],
        );
        let typed = run(vec![Cmd::Press { key: 2, action: Action::Macro(m) }]);
        assert_eq!(typed.len(), 5);
        assert!(typed[4].starts_with("up") && typed[4].contains("code: 42"), "{typed:?}");
    }

    #[test]
    fn a_while_held_macro_stops_on_release() {
        let m = mac(MacroMode::WhileHeld, vec![Step::Media(ButtonAction::VolumeUp)]);
        let (tx, rx) = channel();
        let t = std::thread::spawn(move || {
            let mut e = Engine::new(Record::default(), rx);
            while let Ok(c) = e.rx.recv() {
                if !e.handle(c) {
                    break;
                }
            }
            e.into_out().0
        });
        tx.send(Cmd::Press { key: 3, action: Action::Macro(m) }).unwrap();
        std::thread::sleep(Duration::from_millis(70));
        tx.send(Cmd::Release { key: 3 }).unwrap();
        tx.send(Cmd::Quit).unwrap();
        let typed = t.join().unwrap();
        // Rounds of at least 20 ms: a few, not thousands.
        assert!((2..=6).contains(&typed.len()), "{typed:?}");
    }

    #[test]
    fn other_keys_work_while_a_macro_runs() {
        let m = mac(MacroMode::Toggle, vec![Step::Delay(5)]);
        let (tx, rx) = channel();
        let t = std::thread::spawn(move || {
            let mut e = Engine::new(Record::default(), rx);
            while let Ok(c) = e.rx.recv() {
                if !e.handle(c) {
                    break;
                }
            }
            e.into_out().0
        });
        tx.send(Cmd::Press { key: 4, action: Action::Macro(m) }).unwrap();
        tx.send(Cmd::Press { key: 0, action: Action::Text("x".into()) }).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        // The toggle's second press stops it.
        tx.send(Cmd::Press { key: 4, action: Action::Text(String::new()) }).unwrap();
        tx.send(Cmd::Quit).unwrap();
        assert_eq!(t.join().unwrap(), ["text x"]);
    }
}
