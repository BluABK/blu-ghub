//! The settings window's Macros tab: macros made, named and put together
//! step by step, for the G-keys to run.
//!
//! Renaming a macro renames it where the G-keys use it; one in use cannot
//! be deleted. Nothing here records key presses: blu-ghub watches no
//! keyboard input.

use crate::gkeys::{GAction, GKeys, Macro, MacroMode, Step};
use crate::input::ButtonAction;

use super::settings::{FileAnswer, pick_file};

#[derive(Default)]
pub struct MacroEditor {
    /// The macro shown.
    selected: usize,
    /// An open file dialog: for which step, and its answer.
    browsing: Option<(usize, FileAnswer)>,
    /// The macro shown, and the name the G-keys use for it: a rename
    /// follows into the G-keys only once it is unique, so passing through
    /// another macro's name while typing cannot merge the two.
    name_used: Option<(usize, String)>,
}

const KINDS: [&str; 8] = [
    "Press and release a key",
    "Hold a key down",
    "Let a key go",
    "Type text",
    "Paste text",
    "Paste a file or image",
    "Media or volume key",
    "Wait",
];

fn kind(s: &Step) -> usize {
    match s {
        Step::Tap(_) => 0,
        Step::Down(_) => 1,
        Step::Up(_) => 2,
        Step::Text(_) => 3,
        Step::Paste(_) => 4,
        Step::PasteFile(_) => 5,
        Step::Media(_) => 6,
        Step::Delay(_) => 7,
    }
}

/// A new step of a kind, keeping the key or text of the one it replaces
/// where that fits.
fn step_of_kind(k: usize, old: &Step) -> Step {
    let key = match old {
        Step::Tap(n) | Step::Down(n) | Step::Up(n) => n.clone(),
        _ => "left_ctrl".to_owned(),
    };
    let text = match old {
        Step::Text(t) | Step::Paste(t) => t.clone(),
        _ => String::new(),
    };
    match k {
        0 => Step::Tap(key),
        1 => Step::Down(key),
        2 => Step::Up(key),
        3 => Step::Text(text),
        4 => Step::Paste(text),
        5 => Step::PasteFile(Default::default()),
        6 => Step::Media(ButtonAction::PlayPause),
        _ => Step::Delay(100),
    }
}

/// Every key a step can press: the keyboard's keys and F13-F24.
fn key_names() -> Vec<&'static str> {
    const EXTRA: [&str; 12] =
        ["f13", "f14", "f15", "f16", "f17", "f18", "f19", "f20", "f21", "f22", "f23", "f24"];
    crate::keymap::KEYS
        .iter()
        .filter(|k| crate::inject::key_named(k.name).is_some())
        .map(|k| k.name)
        .chain(EXTRA)
        .collect()
}

/// Where the G-keys use macro `name`, e.g. "M1 G3".
pub fn uses(gkeys: &GKeys, name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (b, bank) in gkeys.banks().iter().enumerate() {
        for (k, a) in bank.keys().iter().enumerate() {
            if matches!(a, GAction::Macro(n) if n == name) {
                out.push(format!("M{} G{}", b + 1, k + 1));
            }
        }
    }
    out
}

fn rename_uses(gkeys: &mut GKeys, from: &str, to: &str) {
    for bank in gkeys.banks_mut() {
        for a in bank.keys_mut() {
            if let GAction::Macro(n) = a
                && n == from
            {
                *n = to.to_owned();
            }
        }
    }
}

/// A name no macro has yet: "Macro 1", "Macro 2", ...
fn fresh_name(macros: &[Macro], base: &str) -> String {
    (1..)
        .map(|i| format!("{base} {i}"))
        .find(|n| !macros.iter().any(|m| m.name == *n))
        .unwrap_or_default()
}

impl MacroEditor {
    pub fn show(&mut self, ui: &mut egui::Ui, macros: &mut Vec<Macro>, gkeys: &mut GKeys) {
        ui.label(
            egui::RichText::new(
                // No arrow: the window's font has none.
                "Macros for the G-keys to run (pick one for a key in the G-keys tab).",
            )
            .weak(),
        );
        ui.add_space(4.0);
        self.selected = self.selected.min(macros.len().saturating_sub(1));
        ui.horizontal(|ui| {
            if !macros.is_empty() {
                egui::ComboBox::from_id_salt("macro-pick")
                    .selected_text(macros[self.selected].name.as_str())
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for (i, m) in macros.iter().enumerate() {
                            ui.selectable_value(&mut self.selected, i, m.name.as_str());
                        }
                    });
            }
            if ui.button("New").clicked() {
                let name = fresh_name(macros, "Macro");
                macros.push(Macro { name, ..Default::default() });
                self.selected = macros.len() - 1;
            }
            if !macros.is_empty() && ui.button("Duplicate").clicked() {
                let mut copy = macros[self.selected].clone();
                copy.name = fresh_name(macros, &copy.name);
                macros.push(copy);
                self.selected = macros.len() - 1;
            }
            if !macros.is_empty() {
                let used = uses(gkeys, &macros[self.selected].name);
                let delete = ui.add_enabled(used.is_empty(), egui::Button::new("Delete"));
                let delete = if used.is_empty() {
                    delete
                } else {
                    delete.on_disabled_hover_text(format!("Used by {}", used.join(", ")))
                };
                if delete.clicked() {
                    macros.remove(self.selected);
                    self.selected = self.selected.saturating_sub(1);
                    self.name_used = None;
                }
            }
        });
        let Some(m) = macros.get(self.selected).cloned() else {
            ui.add_space(8.0);
            ui.label("No macros yet.");
            return;
        };
        let mut m = m;
        // Another macro shown, or no G-key uses the remembered name (after
        // "Undo changes", say): start again from the name it has.
        if self
            .name_used
            .as_ref()
            .is_none_or(|(i, used)| *i != self.selected || uses(gkeys, used).is_empty())
        {
            self.name_used = Some((self.selected, m.name.clone()));
        }
        ui.add_space(8.0);
        let names_taken = |name: &str, macros: &[Macro], me: usize| {
            macros.iter().enumerate().any(|(i, x)| i != me && x.name == name)
        };
        egui::Grid::new("macro-head").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
            ui.label("Name");
            if ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(200.0)).changed()
                && !m.name.is_empty()
                && !names_taken(&m.name, macros, self.selected)
                && let Some((_, used)) = &mut self.name_used
            {
                rename_uses(gkeys, used, &m.name);
                *used = m.name.clone();
            }
            ui.end_row();
            ui.label("Runs");
            egui::ComboBox::from_id_salt("macro-mode")
                .selected_text(m.mode.label())
                .width(200.0)
                .show_ui(ui, |ui| {
                    for mode in MacroMode::ALL {
                        ui.selectable_value(&mut m.mode, mode, mode.label());
                    }
                });
            ui.end_row();
            ui.label("Pause after each step");
            ui.add(egui::DragValue::new(&mut m.delay_ms).range(0..=10_000).suffix(" ms"))
                .on_hover_text("Waits come on top of this.");
            ui.end_row();
        });
        if names_taken(&m.name, macros, self.selected) {
            ui.label(
                egui::RichText::new("Another macro has this name.")
                    .color(ui.visuals().error_fg_color),
            );
        }
        let used = uses(gkeys, &m.name);
        ui.label(
            egui::RichText::new(if used.is_empty() {
                "Not used by any G-key.".to_owned()
            } else {
                format!("Used by {}.", used.join(", "))
            })
            .weak(),
        );

        ui.add_space(8.0);
        ui.strong("Steps");
        let keys = key_names();
        let mut action: Option<(usize, StepEdit)> = None;
        egui::Grid::new("macro-steps").num_columns(3).spacing([10.0, 6.0]).show(ui, |ui| {
            let n = m.steps.len();
            for (i, step) in m.steps.iter_mut().enumerate() {
                ui.label(format!("{}.", i + 1));
                ui.horizontal(|ui| {
                    if step_editor(ui, i, step, &keys) {
                        action = Some((i, StepEdit::Browse));
                    }
                });
                ui.horizontal(|ui| {
                    if ui.add_enabled(i > 0, egui::Button::new("⬆").small()).clicked() {
                        action = Some((i, StepEdit::Up));
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("⬇").small()).clicked() {
                        action = Some((i, StepEdit::Down));
                    }
                    if ui.small_button("✖").on_hover_text("Remove this step").clicked() {
                        action = Some((i, StepEdit::Remove));
                    }
                });
                ui.end_row();
            }
        });
        match action {
            Some((i, StepEdit::Up)) => m.steps.swap(i, i - 1),
            Some((i, StepEdit::Down)) => m.steps.swap(i, i + 1),
            Some((i, StepEdit::Remove)) => {
                m.steps.remove(i);
            }
            Some((i, StepEdit::Browse)) if self.browsing.is_none() => {
                self.browsing = Some((i, pick_file(ui.ctx().clone())));
            }
            _ => {}
        }
        if ui.button("Add step").clicked() {
            let next = m.steps.last().map_or(Step::Tap("a".into()), |s| step_of_kind(kind(s), s));
            m.steps.push(next);
        }
        // The file dialog's answer.
        if let Some((i, rx)) = &self.browsing {
            match rx.try_recv() {
                Ok(picked) => {
                    let i = *i;
                    self.browsing = None;
                    if let Some(path) = picked
                        && let Some(Step::PasteFile(p)) = m.steps.get_mut(i)
                    {
                        *p = path;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.browsing = None,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        for p in m.problems() {
            ui.label(egui::RichText::new(p).color(ui.visuals().error_fg_color));
        }
        macros[self.selected] = m;
    }
}

enum StepEdit {
    Up,
    Down,
    Remove,
    Browse,
}

/// One step: its kind and its key, text, file, media key or wait. `true`
/// when "Browse…" was clicked.
fn step_editor(ui: &mut egui::Ui, i: usize, step: &mut Step, keys: &[&'static str]) -> bool {
    let mut k = kind(step);
    egui::ComboBox::from_id_salt(("step-kind", i)).selected_text(KINDS[k]).width(170.0).show_ui(
        ui,
        |ui| {
            for (j, label) in KINDS.iter().enumerate() {
                ui.selectable_value(&mut k, j, *label);
            }
        },
    );
    if k != kind(step) {
        *step = step_of_kind(k, step);
    }
    let mut browse = false;
    match step {
        Step::Tap(n) | Step::Down(n) | Step::Up(n) => {
            egui::ComboBox::from_id_salt(("step-key", i))
                .selected_text(n.as_str())
                .width(140.0)
                .height(300.0)
                .show_ui(ui, |ui| {
                    for name in keys {
                        ui.selectable_value(n, (*name).to_owned(), *name);
                    }
                });
        }
        Step::Text(t) | Step::Paste(t) => {
            ui.add(egui::TextEdit::singleline(t).desired_width(200.0));
        }
        Step::PasteFile(p) => {
            let mut text = p.to_string_lossy().into_owned();
            if ui
                .add(
                    egui::TextEdit::singleline(&mut text)
                        .desired_width(200.0)
                        .hint_text("file to paste"),
                )
                .changed()
            {
                *p = text.into();
            }
            browse = ui.button("Browse…").clicked();
        }
        Step::Media(a) => {
            egui::ComboBox::from_id_salt(("step-media", i))
                .selected_text(a.label())
                .width(170.0)
                .show_ui(ui, |ui| {
                    for x in ButtonAction::ALL {
                        if !matches!(x, ButtonAction::Default | ButtonAction::Off) {
                            ui.selectable_value(a, x, x.label());
                        }
                    }
                });
        }
        Step::Delay(ms) => {
            ui.add(egui::DragValue::new(ms).range(0..=60_000).suffix(" ms"));
        }
    }
    browse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_name_offered_is_typeable() {
        let names = key_names();
        assert!(names.contains(&"esc") && names.contains(&"f24"));
        assert!(!names.contains(&"logo"));
        assert!(names.iter().all(|n| crate::inject::key_named(n).is_some()));
    }

    #[test]
    fn renaming_follows_into_the_g_keys() {
        let mut g = GKeys::default();
        g.m2.g3 = GAction::Macro("old".into());
        assert_eq!(uses(&g, "old"), ["M2 G3"]);
        rename_uses(&mut g, "old", "new");
        assert_eq!(g.m2.g3, GAction::Macro("new".into()));
        assert!(uses(&g, "old").is_empty());
    }

    #[test]
    fn new_names_do_not_clash() {
        let ms = vec![Macro { name: "Macro 1".into(), ..Default::default() }];
        assert_eq!(fresh_name(&ms, "Macro"), "Macro 2");
    }

    #[test]
    fn changing_a_steps_kind_keeps_its_key_or_text() {
        assert_eq!(step_of_kind(1, &Step::Tap("q".into())), Step::Down("q".into()));
        assert_eq!(step_of_kind(4, &Step::Text("hi".into())), Step::Paste("hi".into()));
        assert_eq!(step_of_kind(7, &Step::Text("hi".into())), Step::Delay(100));
    }
}
