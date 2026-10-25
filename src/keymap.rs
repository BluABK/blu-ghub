//! The G915's per-key lighting zones (0x8081) and the names blu-ghub's
//! config gives them.
//!
//! A main key's zone is its USB HID usage minus 3 (Escape, usage 0x29, is
//! zone 0x26), and the modifiers (usages 0xE0-0xE7) are zones 104-111. The
//! rest come from g810-led's G815 table, the same family: the media keys
//! 0x9B-0x9E, the lighting key 0x99, G1-G5 0xB4-0xB8 and the logo 0xD2.
//!
//! Names follow the HID usage, which is the key's *position* on a US
//! layout, not its label: on a Nordic keyboard `semicolon` is Ø and
//! `non_us_hash` is the ' * key beside Enter.

/// One lit key: its name in the config and its lighting zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub name: &'static str,
    pub zone: u8,
}

const fn usage(name: &'static str, usage: u8) -> Key {
    let zone = if usage >= 0xE0 { usage - 0x78 } else { usage - 3 };
    Key { name, zone }
}

const fn zone(name: &'static str, zone: u8) -> Key {
    Key { name, zone }
}

/// Every key the G915 lights. (The ANSI backslash, usage 0x31, is not on
/// the ISO board.)
pub static KEYS: &[Key] = &[
    usage("a", 0x04),
    usage("b", 0x05),
    usage("c", 0x06),
    usage("d", 0x07),
    usage("e", 0x08),
    usage("f", 0x09),
    usage("g", 0x0A),
    usage("h", 0x0B),
    usage("i", 0x0C),
    usage("j", 0x0D),
    usage("k", 0x0E),
    usage("l", 0x0F),
    usage("m", 0x10),
    usage("n", 0x11),
    usage("o", 0x12),
    usage("p", 0x13),
    usage("q", 0x14),
    usage("r", 0x15),
    usage("s", 0x16),
    usage("t", 0x17),
    usage("u", 0x18),
    usage("v", 0x19),
    usage("w", 0x1A),
    usage("x", 0x1B),
    usage("y", 0x1C),
    usage("z", 0x1D),
    usage("1", 0x1E),
    usage("2", 0x1F),
    usage("3", 0x20),
    usage("4", 0x21),
    usage("5", 0x22),
    usage("6", 0x23),
    usage("7", 0x24),
    usage("8", 0x25),
    usage("9", 0x26),
    usage("0", 0x27),
    usage("enter", 0x28),
    usage("esc", 0x29),
    usage("backspace", 0x2A),
    usage("tab", 0x2B),
    usage("space", 0x2C),
    usage("minus", 0x2D),
    usage("equal", 0x2E),
    usage("left_bracket", 0x2F),
    usage("right_bracket", 0x30),
    usage("non_us_hash", 0x32),
    usage("semicolon", 0x33),
    usage("apostrophe", 0x34),
    usage("grave", 0x35),
    usage("comma", 0x36),
    usage("period", 0x37),
    usage("slash", 0x38),
    usage("caps_lock", 0x39),
    usage("f1", 0x3A),
    usage("f2", 0x3B),
    usage("f3", 0x3C),
    usage("f4", 0x3D),
    usage("f5", 0x3E),
    usage("f6", 0x3F),
    usage("f7", 0x40),
    usage("f8", 0x41),
    usage("f9", 0x42),
    usage("f10", 0x43),
    usage("f11", 0x44),
    usage("f12", 0x45),
    usage("print_screen", 0x46),
    usage("scroll_lock", 0x47),
    usage("pause", 0x48),
    usage("insert", 0x49),
    usage("home", 0x4A),
    usage("page_up", 0x4B),
    usage("delete", 0x4C),
    usage("end", 0x4D),
    usage("page_down", 0x4E),
    usage("right", 0x4F),
    usage("left", 0x50),
    usage("down", 0x51),
    usage("up", 0x52),
    usage("num_lock", 0x53),
    usage("kp_divide", 0x54),
    usage("kp_multiply", 0x55),
    usage("kp_minus", 0x56),
    usage("kp_plus", 0x57),
    usage("kp_enter", 0x58),
    usage("kp_1", 0x59),
    usage("kp_2", 0x5A),
    usage("kp_3", 0x5B),
    usage("kp_4", 0x5C),
    usage("kp_5", 0x5D),
    usage("kp_6", 0x5E),
    usage("kp_7", 0x5F),
    usage("kp_8", 0x60),
    usage("kp_9", 0x61),
    usage("kp_0", 0x62),
    usage("kp_period", 0x63),
    usage("non_us_backslash", 0x64),
    usage("menu", 0x65),
    usage("left_ctrl", 0xE0),
    usage("left_shift", 0xE1),
    usage("left_alt", 0xE2),
    usage("left_win", 0xE3),
    usage("right_ctrl", 0xE4),
    usage("right_shift", 0xE5),
    usage("right_alt", 0xE6),
    usage("right_win", 0xE7),
    zone("lighting", 0x99),
    zone("play_pause", 0x9B),
    zone("mute", 0x9C),
    zone("next_track", 0x9D),
    zone("previous_track", 0x9E),
    zone("g1", 0xB4),
    zone("g2", 0xB5),
    zone("g3", 0xB6),
    zone("g4", 0xB7),
    zone("g5", 0xB8),
    zone("logo", 0xD2),
];

pub fn by_name(name: &str) -> Option<Key> {
    KEYS.iter().find(|k| k.name == name).copied()
}

/// The key a HID keyboard usage lights, if the G915 has it.
pub fn by_usage(u: u32) -> Option<Key> {
    let zone = match u {
        0x04..=0x65 => u - 3,
        0xE0..=0xE7 => u - 0x78,
        _ => return None,
    };
    KEYS.iter().find(|k| u32::from(k.zone) == zone).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_zones_are_unique() {
        for (i, a) in KEYS.iter().enumerate() {
            for b in &KEYS[i + 1..] {
                assert_ne!(a.name, b.name);
                assert_ne!(a.zone, b.zone, "{} and {}", a.name, b.name);
            }
        }
    }

    #[test]
    fn known_zones() {
        assert_eq!(by_name("esc").unwrap().zone, 0x26);
        assert_eq!(by_name("f1").unwrap().zone, 0x37);
        assert_eq!(by_name("left_ctrl").unwrap().zone, 104);
        assert_eq!(by_name("right_win").unwrap().zone, 111);
        assert_eq!(by_usage(0x29).unwrap().name, "esc");
        assert_eq!(by_usage(0xE1).unwrap().name, "left_shift");
    }

    #[test]
    fn the_ansi_backslash_is_not_on_the_board() {
        assert_eq!(by_usage(0x31), None);
        assert_eq!(KEYS.len(), 105 + 4 + 5 + 2);
    }
}
