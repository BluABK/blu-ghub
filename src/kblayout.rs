//! Where the G915's lit keys sit, for drawing it: a full-size ISO board with
//! Nordic labels, the G-key column on the left and the round buttons along
//! the top. In key units (1 = a letter key's pitch), from the top left.

use crate::keymap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cap {
    /// The key's name in [`keymap`].
    pub name: &'static str,
    pub label: &'static str,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Drawn round (the buttons along the top).
    pub round: bool,
}

/// Width and height of the whole board.
pub const SIZE: (f32, f32) = (24.0, 7.5);

/// The main block's left edge, after the G-key column.
const X0: f32 = 1.5;
/// The navigation cluster and the numpad.
const NAV: f32 = X0 + 15.25;
const PAD: f32 = X0 + 18.5;
/// Rows: the buttons, the F-keys, then the five main rows.
const TOP: f32 = 0.0;
const FN: f32 = 1.25;
const R1: f32 = 2.5;

const fn key(name: &'static str, label: &'static str, x: f32, y: f32, w: f32) -> Cap {
    Cap { name, label, x, y, w, h: 1.0, round: false }
}

const fn tall(name: &'static str, label: &'static str, x: f32, y: f32) -> Cap {
    Cap { name, label, x, y, w: 1.0, h: 2.0, round: false }
}

const fn button(name: &'static str, label: &'static str, x: f32) -> Cap {
    Cap { name, label, x, y: TOP, w: 1.0, h: 1.0, round: true }
}

pub static CAPS: &[Cap] = &[
    // Along the top.
    button("logo", "G", 0.0),
    button("lighting", "☀", X0 + 9.75),
    button("previous_track", "⏮", PAD),
    button("play_pause", "⏯", PAD + 1.0),
    button("next_track", "⏭", PAD + 2.0),
    button("mute", "🔇", PAD + 3.0),
    // F-row.
    key("esc", "Esc", X0, FN, 1.0),
    key("f1", "F1", X0 + 2.0, FN, 1.0),
    key("f2", "F2", X0 + 3.0, FN, 1.0),
    key("f3", "F3", X0 + 4.0, FN, 1.0),
    key("f4", "F4", X0 + 5.0, FN, 1.0),
    key("f5", "F5", X0 + 6.5, FN, 1.0),
    key("f6", "F6", X0 + 7.5, FN, 1.0),
    key("f7", "F7", X0 + 8.5, FN, 1.0),
    key("f8", "F8", X0 + 9.5, FN, 1.0),
    key("f9", "F9", X0 + 11.0, FN, 1.0),
    key("f10", "F10", X0 + 12.0, FN, 1.0),
    key("f11", "F11", X0 + 13.0, FN, 1.0),
    key("f12", "F12", X0 + 14.0, FN, 1.0),
    key("print_screen", "PrtSc", NAV, FN, 1.0),
    key("scroll_lock", "ScrLk", NAV + 1.0, FN, 1.0),
    key("pause", "Pause", NAV + 2.0, FN, 1.0),
    // G-keys.
    key("g1", "G1", 0.0, R1, 1.0),
    key("g2", "G2", 0.0, R1 + 1.0, 1.0),
    key("g3", "G3", 0.0, R1 + 2.0, 1.0),
    key("g4", "G4", 0.0, R1 + 3.0, 1.0),
    key("g5", "G5", 0.0, R1 + 4.0, 1.0),
    // Number row.
    key("grave", "§", X0, R1, 1.0),
    key("1", "1", X0 + 1.0, R1, 1.0),
    key("2", "2", X0 + 2.0, R1, 1.0),
    key("3", "3", X0 + 3.0, R1, 1.0),
    key("4", "4", X0 + 4.0, R1, 1.0),
    key("5", "5", X0 + 5.0, R1, 1.0),
    key("6", "6", X0 + 6.0, R1, 1.0),
    key("7", "7", X0 + 7.0, R1, 1.0),
    key("8", "8", X0 + 8.0, R1, 1.0),
    key("9", "9", X0 + 9.0, R1, 1.0),
    key("0", "0", X0 + 10.0, R1, 1.0),
    key("minus", "+", X0 + 11.0, R1, 1.0),
    key("equal", "\\", X0 + 12.0, R1, 1.0),
    key("backspace", "Bksp", X0 + 13.0, R1, 2.0),
    key("insert", "Ins", NAV, R1, 1.0),
    key("home", "Home", NAV + 1.0, R1, 1.0),
    key("page_up", "PgUp", NAV + 2.0, R1, 1.0),
    key("num_lock", "Num", PAD, R1, 1.0),
    key("kp_divide", "/", PAD + 1.0, R1, 1.0),
    key("kp_multiply", "*", PAD + 2.0, R1, 1.0),
    key("kp_minus", "-", PAD + 3.0, R1, 1.0),
    // Tab row.
    key("tab", "Tab", X0, R1 + 1.0, 1.5),
    key("q", "Q", X0 + 1.5, R1 + 1.0, 1.0),
    key("w", "W", X0 + 2.5, R1 + 1.0, 1.0),
    key("e", "E", X0 + 3.5, R1 + 1.0, 1.0),
    key("r", "R", X0 + 4.5, R1 + 1.0, 1.0),
    key("t", "T", X0 + 5.5, R1 + 1.0, 1.0),
    key("y", "Y", X0 + 6.5, R1 + 1.0, 1.0),
    key("u", "U", X0 + 7.5, R1 + 1.0, 1.0),
    key("i", "I", X0 + 8.5, R1 + 1.0, 1.0),
    key("o", "O", X0 + 9.5, R1 + 1.0, 1.0),
    key("p", "P", X0 + 10.5, R1 + 1.0, 1.0),
    key("left_bracket", "Å", X0 + 11.5, R1 + 1.0, 1.0),
    key("right_bracket", "¨", X0 + 12.5, R1 + 1.0, 1.0),
    Cap {
        name: "enter",
        label: "Enter",
        x: X0 + 13.75,
        y: R1 + 1.0,
        w: 1.25,
        h: 2.0,
        round: false,
    },
    key("delete", "Del", NAV, R1 + 1.0, 1.0),
    key("end", "End", NAV + 1.0, R1 + 1.0, 1.0),
    key("page_down", "PgDn", NAV + 2.0, R1 + 1.0, 1.0),
    key("kp_7", "7", PAD, R1 + 1.0, 1.0),
    key("kp_8", "8", PAD + 1.0, R1 + 1.0, 1.0),
    key("kp_9", "9", PAD + 2.0, R1 + 1.0, 1.0),
    tall("kp_plus", "+", PAD + 3.0, R1 + 1.0),
    // Caps row.
    key("caps_lock", "Caps", X0, R1 + 2.0, 1.75),
    key("a", "A", X0 + 1.75, R1 + 2.0, 1.0),
    key("s", "S", X0 + 2.75, R1 + 2.0, 1.0),
    key("d", "D", X0 + 3.75, R1 + 2.0, 1.0),
    key("f", "F", X0 + 4.75, R1 + 2.0, 1.0),
    key("g", "G", X0 + 5.75, R1 + 2.0, 1.0),
    key("h", "H", X0 + 6.75, R1 + 2.0, 1.0),
    key("j", "J", X0 + 7.75, R1 + 2.0, 1.0),
    key("k", "K", X0 + 8.75, R1 + 2.0, 1.0),
    key("l", "L", X0 + 9.75, R1 + 2.0, 1.0),
    key("semicolon", "Ø", X0 + 10.75, R1 + 2.0, 1.0),
    key("apostrophe", "Æ", X0 + 11.75, R1 + 2.0, 1.0),
    key("non_us_hash", "'", X0 + 12.75, R1 + 2.0, 1.0),
    key("kp_4", "4", PAD, R1 + 2.0, 1.0),
    key("kp_5", "5", PAD + 1.0, R1 + 2.0, 1.0),
    key("kp_6", "6", PAD + 2.0, R1 + 2.0, 1.0),
    // Shift row.
    key("left_shift", "Shift", X0, R1 + 3.0, 1.25),
    key("non_us_backslash", "<", X0 + 1.25, R1 + 3.0, 1.0),
    key("z", "Z", X0 + 2.25, R1 + 3.0, 1.0),
    key("x", "X", X0 + 3.25, R1 + 3.0, 1.0),
    key("c", "C", X0 + 4.25, R1 + 3.0, 1.0),
    key("v", "V", X0 + 5.25, R1 + 3.0, 1.0),
    key("b", "B", X0 + 6.25, R1 + 3.0, 1.0),
    key("n", "N", X0 + 7.25, R1 + 3.0, 1.0),
    key("m", "M", X0 + 8.25, R1 + 3.0, 1.0),
    key("comma", ",", X0 + 9.25, R1 + 3.0, 1.0),
    key("period", ".", X0 + 10.25, R1 + 3.0, 1.0),
    key("slash", "-", X0 + 11.25, R1 + 3.0, 1.0),
    key("right_shift", "Shift", X0 + 12.25, R1 + 3.0, 2.75),
    key("up", "⬆", NAV + 1.0, R1 + 3.0, 1.0),
    key("kp_1", "1", PAD, R1 + 3.0, 1.0),
    key("kp_2", "2", PAD + 1.0, R1 + 3.0, 1.0),
    key("kp_3", "3", PAD + 2.0, R1 + 3.0, 1.0),
    tall("kp_enter", "Ent", PAD + 3.0, R1 + 3.0),
    // Bottom row.
    key("left_ctrl", "Ctrl", X0, R1 + 4.0, 1.25),
    key("left_win", "Win", X0 + 1.25, R1 + 4.0, 1.25),
    key("left_alt", "Alt", X0 + 2.5, R1 + 4.0, 1.25),
    key("space", "", X0 + 3.75, R1 + 4.0, 6.25),
    key("right_alt", "AltGr", X0 + 10.0, R1 + 4.0, 1.25),
    key("right_win", "Win", X0 + 11.25, R1 + 4.0, 1.25),
    key("menu", "Menu", X0 + 12.5, R1 + 4.0, 1.25),
    key("right_ctrl", "Ctrl", X0 + 13.75, R1 + 4.0, 1.25),
    key("left", "⬅", NAV, R1 + 4.0, 1.0),
    key("down", "⬇", NAV + 1.0, R1 + 4.0, 1.0),
    key("right", "➡", NAV + 2.0, R1 + 4.0, 1.0),
    key("kp_0", "0", PAD, R1 + 4.0, 2.0),
    key("kp_period", ",", PAD + 2.0, R1 + 4.0, 1.0),
];

/// The zone of a cap's key.
pub fn zone(cap: &Cap) -> u8 {
    keymap::by_name(cap.name).map_or(0, |k| k.zone)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_lit_key_is_drawn_once() {
        for k in keymap::KEYS {
            assert_eq!(CAPS.iter().filter(|c| c.name == k.name).count(), 1, "{}", k.name);
        }
        assert_eq!(CAPS.len(), keymap::KEYS.len());
    }

    #[test]
    fn no_two_caps_overlap_and_all_fit() {
        for (i, a) in CAPS.iter().enumerate() {
            assert!(a.x >= 0.0 && a.y >= 0.0, "{}", a.name);
            assert!(a.x + a.w <= SIZE.0 && a.y + a.h <= SIZE.1, "{}", a.name);
            for b in &CAPS[i + 1..] {
                let apart =
                    a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "{} and {}", a.name, b.name);
            }
        }
    }
}
