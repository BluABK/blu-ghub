//! The equaliser as a curve: a point per band, dragged up or down in steps
//! of `step` dB, double-clicked back to 0. Under the bands, what part of
//! the sound they are (bass, mids, treble); hovering a band says more.

use egui::{Align2, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

/// What happened this frame.
#[derive(Default)]
pub struct EqResponse {
    /// A gain changed (while dragging, too).
    pub changed: bool,
    /// A drag ended or a band was reset: the moment to let the headset hear it.
    pub settled: bool,
}

/// Draw `gains` over `bands` (Hz), each kept within `range` dB in steps of
/// `step`; editable unless `enabled` is false. `drag` holds the band being
/// dragged between frames.
pub fn show(
    ui: &mut egui::Ui,
    bands: &[f32],
    gains: &mut [f32],
    range: (f32, f32),
    step: f32,
    enabled: bool,
    drag: &mut Option<usize>,
) -> EqResponse {
    let mut out = EqResponse::default();
    let n = bands.len().min(gains.len());
    let width = ui.available_width().min(640.0);
    let (area, resp) = ui.allocate_exact_size(
        vec2(width, 214.0),
        if enabled { Sense::click_and_drag() } else { Sense::hover() },
    );
    // The plot inside: room for dB marks on the left, band and region
    // names below.
    let plot = Rect::from_min_max(area.min + vec2(40.0, 16.0), area.max - vec2(8.0, 46.0));
    let (lo, hi) = range;
    let x = |i: usize| plot.left() + (i as f32 + 0.5) * plot.width() / n.max(1) as f32;
    let y = |db: f32| plot.top() + (hi - db) / (hi - lo).max(1.0) * plot.height();
    let db_at = |py: f32| hi - (py - plot.top()) / plot.height() * (hi - lo);
    let nearest =
        |p: Pos2| (0..n).min_by(|a, b| (x(*a) - p.x).abs().total_cmp(&(x(*b) - p.x).abs()));

    // Dragging and resetting.
    if enabled && n > 0 {
        if resp.drag_started() {
            *drag = resp.interact_pointer_pos().and_then(nearest);
        }
        if let (Some(i), Some(p)) = (*drag, resp.interact_pointer_pos())
            && resp.dragged()
        {
            let g = ((db_at(p.y) / step).round() * step).clamp(lo, hi);
            if gains[i] != g {
                gains[i] = g;
                out.changed = true;
            }
        }
        if resp.drag_stopped() {
            *drag = None;
            out.settled = true;
        }
        if resp.double_clicked()
            && let Some(i) = resp.interact_pointer_pos().and_then(nearest)
            && gains[i] != 0.0
        {
            gains[i] = 0.0;
            out.changed = true;
            out.settled = true;
        }
    }

    // Drawing.
    let painter = ui.painter_at(area);
    let v = ui.visuals();
    let line = if enabled { v.selection.stroke.color } else { v.weak_text_color() };
    let grid = v.widgets.noninteractive.bg_stroke.color;
    let text = v.weak_text_color();
    painter.rect(plot, 4.0, v.extreme_bg_color, Stroke::new(1.0, grid), StrokeKind::Inside);
    // dB marks: the range's ends, half way, and 0.
    let mark = ((hi - lo) / 4.0).max(1.0);
    let mut db = lo;
    while db <= hi + 0.01 {
        let yy = y(db);
        let s = if db.abs() < 0.01 { Stroke::new(1.0, text) } else { Stroke::new(0.5, grid) };
        painter.line_segment([pos2(plot.left(), yy), pos2(plot.right(), yy)], s);
        painter.text(
            pos2(plot.left() - 6.0, yy),
            Align2::RIGHT_CENTER,
            format!("{db:+.0}"),
            FontId::proportional(11.0),
            text,
        );
        db += mark;
    }
    if n == 0 {
        return out;
    }
    // The curve, and the area between it and 0.
    let points: Vec<Pos2> = (0..n).map(|i| pos2(x(i), y(gains[i]))).collect();
    let zero = y(0.0);
    for w in points.windows(2) {
        let fill = line.gamma_multiply(0.15);
        painter.add(egui::Shape::convex_polygon(
            vec![w[0], w[1], pos2(w[1].x, zero), pos2(w[0].x, zero)],
            fill,
            Stroke::NONE,
        ));
    }
    painter.add(egui::Shape::line(points.clone(), Stroke::new(2.0, line)));
    let hovered = resp.hover_pos().and_then(nearest);
    for (i, p) in points.iter().enumerate() {
        let active = *drag == Some(i) || (enabled && hovered == Some(i));
        let r = if active { 7.0 } else { 5.0 };
        painter.circle(
            *p,
            r,
            if active { line } else { v.extreme_bg_color },
            Stroke::new(2.0, line),
        );
        if active || gains[i] != 0.0 {
            painter.text(
                *p - vec2(0.0, 12.0),
                Align2::CENTER_BOTTOM,
                format!("{:+.1}", gains[i]),
                FontId::proportional(11.0),
                if active { v.strong_text_color() } else { text },
            );
        }
        painter.text(
            pos2(p.x, plot.bottom() + 4.0),
            Align2::CENTER_TOP,
            short_band(bands[i]),
            FontId::proportional(11.0),
            text,
        );
    }
    // The regions: a bracket under the bands each covers, named, and a
    // faint line in the plot where one ends.
    let col = plot.width() / n as f32;
    let mut start = 0;
    for i in 0..n {
        let name = region(bands[i]).0;
        if i + 1 < n && region(bands[i + 1]).0 == name {
            continue;
        }
        let (l, r) = (x(start) - col / 2.0 + 4.0, x(i) + col / 2.0 - 4.0);
        let yy = plot.bottom() + 22.0;
        let s = Stroke::new(1.0, grid);
        painter.line_segment([pos2(l, yy), pos2(r, yy)], s);
        painter.line_segment([pos2(l, yy - 3.0), pos2(l, yy)], s);
        painter.line_segment([pos2(r, yy - 3.0), pos2(r, yy)], s);
        painter.text(
            pos2((l + r) / 2.0, yy + 2.0),
            Align2::CENTER_TOP,
            name,
            FontId::proportional(11.0),
            text,
        );
        if i + 1 < n {
            let xx = x(i) + col / 2.0;
            painter.line_segment(
                [pos2(xx, plot.top() + 1.0), pos2(xx, plot.bottom() - 1.0)],
                Stroke::new(0.5, grid),
            );
        }
        start = i + 1;
    }
    if drag.is_none()
        && let Some(i) = hovered
    {
        let (name, about) = region(bands[i]);
        resp.on_hover_text_at_pointer(format!(
            "{}: {name}, {about}. {:+.1} dB.",
            super::details::band_name(bands[i]),
            gains[i] + 0.0
        ));
    }
    out
}

/// The parts of the range, from where each starts (Hz): a name short enough
/// to go under a band, and what is heard there.
const REGIONS: [(f32, &str, &str); 8] = [
    (0.0, "Sub-bass", "rumble, felt more than heard"),
    (60.0, "Bass", "punch: kick drums, bass lines"),
    (250.0, "Low mids", "warmth; too much sounds muddy"),
    (500.0, "Mids", "the body of voices and instruments"),
    (2000.0, "High mids", "clarity and attack; too much sounds harsh"),
    (4000.0, "Presence", "definition: consonants, footsteps"),
    (6000.0, "Treble", "brightness: cymbals, the hiss of S sounds"),
    (10000.0, "Air", "sparkle and space"),
];

/// The region `hz` is in: its name, and what is heard there.
pub fn region(hz: f32) -> (&'static str, &'static str) {
    let r = REGIONS.iter().rev().find(|r| hz >= r.0).unwrap_or(&REGIONS[0]);
    (r.1, r.2)
}

/// "40", "1.2k", "12.5k": short enough for under a point.
fn short_band(hz: f32) -> String {
    if hz >= 1000.0 { format!("{:.1}k", hz / 1000.0) } else { format!("{hz:.0}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bands_are_named_short() {
        assert_eq!(short_band(40.0), "40");
        assert_eq!(short_band(1183.0), "1.2k");
        assert_eq!(short_band(12507.0), "12.5k");
    }

    #[test]
    fn bands_fall_in_regions() {
        assert_eq!(region(20.0).0, "Sub-bass");
        assert_eq!(region(60.0).0, "Bass");
        assert_eq!(region(1000.0).0, "Mids");
        assert_eq!(region(3000.0).0, "High mids");
        assert_eq!(region(16000.0).0, "Air");
    }
}
