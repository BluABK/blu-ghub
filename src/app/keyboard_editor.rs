//! The G915 layout editor in the settings window: the keyboard drawn with
//! its colours, keys picked by clicking or dragging, and a colour put on the
//! picked ones.
//!
//! Click picks a key, Ctrl- or Shift-click adds or removes one, dragging
//! picks every key the box touches (with Ctrl or Shift, adds them), and a
//! double click picks every key of that colour. Changing the colour paints
//! the picked keys at once.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, vec2};

use crate::kblayout::{self, CAPS, Cap};
use crate::lighting::Rgb;

/// At most this often a changed layout goes to the keyboard.
const PREVIEW_EVERY: Duration = Duration::from_millis(100);

pub struct Editor {
    selected: BTreeSet<&'static str>,
    paint: Rgb,
    hex: String,
    drag_from: Option<Pos2>,
    /// Show the layout on the keyboard while the window has the focus.
    pub preview: bool,
    sent: Option<Vec<(u8, Rgb)>>,
    sent_at: Option<Instant>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            selected: BTreeSet::new(),
            paint: Rgb([0xFF, 0xFF, 0xFF]),
            hex: String::new(),
            drag_from: None,
            preview: true,
            sent: None,
            sent_at: None,
        }
    }
}

impl Editor {
    pub fn show(&mut self, ui: &mut egui::Ui, keys: &mut BTreeMap<String, Rgb>) {
        let u = (ui.available_width() / kblayout::SIZE.0).clamp(18.0, 46.0);
        let (area, resp) = ui.allocate_exact_size(
            vec2(kblayout::SIZE.0 * u, kblayout::SIZE.1 * u),
            Sense::click_and_drag(),
        );
        let cap_rect = |c: &Cap| {
            Rect::from_min_size(area.min + vec2(c.x * u, c.y * u), vec2(c.w * u, c.h * u))
                .shrink(u * 0.06)
        };
        let under = |p: Pos2| CAPS.iter().find(|c| cap_rect(c).contains(p));
        let mods = ui.input(|i| i.modifiers);
        let adding = mods.ctrl || mods.shift || mods.command;

        // Picking.
        if resp.drag_started() {
            self.drag_from = resp.interact_pointer_pos();
        }
        let drag_box = match (self.drag_from, resp.interact_pointer_pos()) {
            (Some(a), Some(b)) if resp.dragged() || resp.drag_stopped() => {
                Some(Rect::from_two_pos(a, b))
            }
            _ => None,
        };
        if resp.drag_stopped() {
            if let Some(b) = drag_box {
                if !adding {
                    self.selected.clear();
                }
                self.selected
                    .extend(CAPS.iter().filter(|c| cap_rect(c).intersects(b)).map(|c| c.name));
            }
            self.drag_from = None;
        } else if resp.double_clicked() {
            if let Some(c) = resp.interact_pointer_pos().and_then(under)
                && let Some(colour) = keys.get(c.name).copied()
            {
                self.selected = keys
                    .iter()
                    .filter(|(_, v)| **v == colour)
                    .filter_map(|(k, _)| name(k))
                    .collect();
                self.set_paint(colour);
            }
        } else if resp.clicked() {
            match resp.interact_pointer_pos().and_then(under) {
                Some(c) if adding => {
                    if !self.selected.remove(c.name) {
                        self.selected.insert(c.name);
                    }
                }
                Some(c) => {
                    self.selected = BTreeSet::from([c.name]);
                    if let Some(colour) = keys.get(c.name) {
                        self.set_paint(*colour);
                    }
                }
                None if !adding => self.selected.clear(),
                None => {}
            }
        }

        // Drawing.
        let painter = ui.painter_at(area);
        let hovered = resp.hover_pos().and_then(under);
        let accent = ui.visuals().selection.stroke.color;
        let edge = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let unset = ui.visuals().extreme_bg_color;
        for c in CAPS {
            let r = cap_rect(c);
            let colour = keys.get(c.name).copied();
            let fill = colour.map_or(unset, |Rgb([r, g, b])| Color32::from_rgb(r, g, b));
            let stroke = if self.selected.contains(c.name) {
                Stroke::new(3.0, accent)
            } else if hovered.is_some_and(|h| h.name == c.name) {
                Stroke::new(1.5, ui.visuals().strong_text_color())
            } else {
                Stroke::new(1.0, edge)
            };
            if c.round {
                let radius = r.width().min(r.height()) * 0.42;
                painter.circle(r.center(), radius, fill, stroke);
            } else {
                painter.rect(r, u * 0.12, fill, stroke, StrokeKind::Inside);
            }
            let text = match colour {
                Some(Rgb([r, g, b])) if luma(r, g, b) > 140.0 => Color32::BLACK,
                Some(_) => Color32::WHITE,
                None => ui.visuals().weak_text_color(),
            };
            let size = if c.label.chars().count() > 3 { u * 0.26 } else { u * 0.36 };
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                c.label,
                FontId::proportional(size),
                text,
            );
        }
        if let Some(b) = drag_box {
            painter.rect(
                b,
                0.0,
                accent.gamma_multiply(0.15),
                Stroke::new(1.0, accent),
                StrokeKind::Inside,
            );
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(match hovered {
                Some(c) => match keys.get(c.name) {
                    Some(colour) => format!("{}: {colour}", c.name),
                    None => format!("{}: not in the layout", c.name),
                },
                None => {
                    "Click, Ctrl-click or drag to pick keys; double-click picks a colour's keys."
                        .to_owned()
                }
            });
        });

        // The colour.
        ui.add_space(4.0);
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label("Colour");
            changed |= egui::widgets::color_picker::color_edit_button_srgb(ui, &mut self.paint.0)
                .changed();
            let typed = ui.add(
                egui::TextEdit::singleline(&mut self.hex)
                    .desired_width(70.0)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("#rrggbb"),
            );
            if typed.changed()
                && let Ok(c) = self.hex.parse::<Rgb>()
            {
                self.paint = c;
                changed = true;
            }
            if !typed.has_focus() {
                self.hex = self.paint.to_string();
            }
            ui.label(match self.selected.len() {
                0 => "no keys picked".to_owned(),
                1 => "1 key picked".to_owned(),
                n => format!("{n} keys picked"),
            });
        });
        ui.horizontal(|ui| {
            if ui.button("Pick all").clicked() {
                self.selected = CAPS.iter().map(|c| c.name).collect();
            }
            if ui.button("Pick none").clicked() {
                self.selected.clear();
            }
            if ui.button("Pick this colour's keys").clicked() {
                let paint = self.paint;
                self.selected = keys
                    .iter()
                    .filter(|(_, v)| **v == paint)
                    .filter_map(|(k, _)| name(k))
                    .collect();
            }
        });

        // The layout's colours, most used first.
        let mut counts: BTreeMap<Rgb, usize> = BTreeMap::new();
        for c in keys.values() {
            *counts.entry(*c).or_default() += 1;
        }
        let mut palette: Vec<(Rgb, usize)> = counts.into_iter().collect();
        palette.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        if !palette.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("In use");
                for (colour, n) in palette {
                    let Rgb([r, g, b]) = colour;
                    let swatch = egui::Button::new("")
                        .fill(Color32::from_rgb(r, g, b))
                        .min_size(vec2(20.0, 20.0));
                    let keys_word = if n == 1 { "key" } else { "keys" };
                    if ui.add(swatch).on_hover_text(format!("{colour}, {n} {keys_word}")).clicked()
                    {
                        self.paint = colour;
                        changed = true;
                    }
                }
            });
        }

        if changed {
            for k in &self.selected {
                keys.insert((*k).to_owned(), self.paint);
            }
        }
    }

    fn set_paint(&mut self, c: Rgb) {
        self.paint = c;
        self.hex = c.to_string();
    }

    /// Hand the keyboard what it should show (`None`: nothing), at most every
    /// [`PREVIEW_EVERY`]; asks for a later frame when it has to hold back.
    pub fn send_preview(&mut self, ctx: &egui::Context, wanted: Option<Vec<(u8, Rgb)>>) {
        if wanted == self.sent {
            return;
        }
        // Stopping is never held back.
        if wanted.is_some()
            && let Some(at) = self.sent_at
            && at.elapsed() < PREVIEW_EVERY
        {
            ctx.request_repaint_after(PREVIEW_EVERY - at.elapsed());
            return;
        }
        super::devices::preview(wanted.clone());
        self.sent = wanted;
        self.sent_at = Some(Instant::now());
    }
}

/// The layout as (zone, colour), for the keyboard.
pub fn frame(keys: &BTreeMap<String, Rgb>) -> Vec<(u8, Rgb)> {
    keys.iter().filter_map(|(k, c)| crate::keymap::by_name(k).map(|key| (key.zone, *c))).collect()
}

/// The `'static` name of a key in the layout.
fn name(k: &str) -> Option<&'static str> {
    crate::keymap::by_name(k).map(|key| key.name)
}

fn luma(r: u8, g: u8, b: u8) -> f32 {
    0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
}
