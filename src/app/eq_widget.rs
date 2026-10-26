//! The equaliser as a curve: a point per band, dragged up or down in steps
//! of `step` dB, double-clicked back to 0.

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
    let width = ui.available_width().min(560.0);
    let (area, resp) = ui.allocate_exact_size(
        vec2(width, 190.0),
        if enabled { Sense::click_and_drag() } else { Sense::hover() },
    );
    // The plot inside: room for dB marks on the left, band names below.
    let plot = Rect::from_min_max(area.min + vec2(40.0, 16.0), area.max - vec2(8.0, 22.0));
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
    out
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
}
