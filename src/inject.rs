//! Typing for the G-keys: keys by scan code, text as Unicode, media keys.
//!
//! Keys go in as set-1 scan codes (what the keyboard itself would send), so
//! Windows applies the keyboard layout to them as to real typing: on a
//! Nordic layout the key at `semicolon` types Ø. Text goes in as Unicode
//! characters instead, whatever the layout. Windows drops synthesised input
//! while an elevated window has the focus, and while the PC is locked.

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, SendInput, VIRTUAL_KEY, VK_CONTROL,
    VK_PAUSE, VK_V,
};

use crate::keymap;

/// A key as typed: a set-1 scan code, `E0`-prefixed (extended) or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    Scan {
        code: u16,
        extended: bool,
    },
    /// Pause has no plain scan code (it is `E1 1D 45`); sent as a virtual key.
    Pause,
}

/// The key for a HID keyboard usage, if it has one.
pub fn from_usage(usage: u8) -> Option<Key> {
    let plain = |code| Some(Key::Scan { code, extended: false });
    let ext = |code| Some(Key::Scan { code, extended: true });
    match usage {
        0x04 => plain(0x1E),
        0x05 => plain(0x30),
        0x06 => plain(0x2E),
        0x07 => plain(0x20),
        0x08 => plain(0x12),
        0x09 => plain(0x21),
        0x0A => plain(0x22),
        0x0B => plain(0x23),
        0x0C => plain(0x17),
        0x0D => plain(0x24),
        0x0E => plain(0x25),
        0x0F => plain(0x26),
        0x10 => plain(0x32),
        0x11 => plain(0x31),
        0x12 => plain(0x18),
        0x13 => plain(0x19),
        0x14 => plain(0x10),
        0x15 => plain(0x13),
        0x16 => plain(0x1F),
        0x17 => plain(0x14),
        0x18 => plain(0x16),
        0x19 => plain(0x2F),
        0x1A => plain(0x11),
        0x1B => plain(0x2D),
        0x1C => plain(0x15),
        0x1D => plain(0x2C),
        // 1-9, 0.
        0x1E..=0x27 => plain(0x02 + (usage - 0x1E) as u16),
        0x28 => plain(0x1C),
        0x29 => plain(0x01),
        0x2A => plain(0x0E),
        0x2B => plain(0x0F),
        0x2C => plain(0x39),
        0x2D => plain(0x0C),
        0x2E => plain(0x0D),
        0x2F => plain(0x1A),
        0x30 => plain(0x1B),
        0x31 | 0x32 => plain(0x2B),
        0x33 => plain(0x27),
        0x34 => plain(0x28),
        0x35 => plain(0x29),
        0x36 => plain(0x33),
        0x37 => plain(0x34),
        0x38 => plain(0x35),
        0x39 => plain(0x3A),
        // F1-F10, F11, F12.
        0x3A..=0x43 => plain(0x3B + (usage - 0x3A) as u16),
        0x44 => plain(0x57),
        0x45 => plain(0x58),
        0x46 => ext(0x37),
        0x47 => plain(0x46),
        0x48 => Some(Key::Pause),
        0x49 => ext(0x52),
        0x4A => ext(0x47),
        0x4B => ext(0x49),
        0x4C => ext(0x53),
        0x4D => ext(0x4F),
        0x4E => ext(0x51),
        0x4F => ext(0x4D),
        0x50 => ext(0x4B),
        0x51 => ext(0x50),
        0x52 => ext(0x48),
        0x53 => plain(0x45),
        0x54 => ext(0x35),
        0x55 => plain(0x37),
        0x56 => plain(0x4A),
        0x57 => plain(0x4E),
        0x58 => ext(0x1C),
        0x59 => plain(0x4F),
        0x5A => plain(0x50),
        0x5B => plain(0x51),
        0x5C => plain(0x4B),
        0x5D => plain(0x4C),
        0x5E => plain(0x4D),
        0x5F => plain(0x47),
        0x60 => plain(0x48),
        0x61 => plain(0x49),
        0x62 => plain(0x52),
        0x63 => plain(0x53),
        0x64 => plain(0x56),
        0x65 => ext(0x5D),
        // F13-F23, F24.
        0x68..=0x72 => plain(0x64 + (usage - 0x68) as u16),
        0x73 => plain(0x76),
        0xE0 => plain(0x1D),
        0xE1 => plain(0x2A),
        0xE2 => plain(0x38),
        0xE3 => ext(0x5B),
        0xE4 => ext(0x1D),
        0xE5 => plain(0x36),
        0xE6 => ext(0x38),
        0xE7 => ext(0x5C),
        _ => None,
    }
}

/// The HID usage of a key name: the config's names for the keyboard's keys
/// ([`keymap`]), plus `f13`-`f24`.
pub fn usage_of(name: &str) -> Option<u8> {
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (13..=24).contains(&n)
    {
        return Some(0x68 + n - 13);
    }
    let zone = keymap::by_name(name)?.zone;
    match zone {
        0x01..=0x62 => Some(zone + 3),
        104..=111 => Some(zone + 0x78),
        _ => None,
    }
}

/// The key a name stands for.
pub fn key_named(name: &str) -> Option<Key> {
    usage_of(name).and_then(from_usage)
}

/// The modifier keys in an on-board binding's modifier bits (bit 0 left
/// Ctrl ... bit 7 right Win), in that order.
pub fn modifiers(bits: u8) -> Vec<Key> {
    (0..8).filter(|b| bits & (1 << b) != 0).filter_map(|b| from_usage(0xE0 + b)).collect()
}

fn input(vk: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn key_input(k: Key, up: bool) -> INPUT {
    let up_flag = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    match k {
        Key::Scan { code, extended } => {
            let ext = if extended { KEYEVENTF_EXTENDEDKEY } else { KEYBD_EVENT_FLAGS(0) };
            input(VIRTUAL_KEY(0), code, KEYEVENTF_SCANCODE | ext | up_flag)
        }
        Key::Pause => input(VK_PAUSE, 0, up_flag),
    }
}

fn send(inputs: &[INPUT]) -> std::io::Result<()> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// Ctrl+V, by virtual key: paste, whatever the layout.
pub fn ctrl_v() -> std::io::Result<()> {
    let up = KEYEVENTF_KEYUP;
    let none = KEYBD_EVENT_FLAGS(0);
    send(&[
        input(VK_CONTROL, 0, none),
        input(VK_V, 0, none),
        input(VK_V, 0, up),
        input(VK_CONTROL, 0, up),
    ])
}

pub fn down(k: Key) -> std::io::Result<()> {
    send(&[key_input(k, false)])
}

pub fn up(k: Key) -> std::io::Result<()> {
    send(&[key_input(k, true)])
}

/// Type `text`: each character as Unicode (two UTF-16 units for one outside
/// the basic plane), except a line break, which presses Enter, and a tab,
/// which presses Tab. In one go: Windows takes it whole.
///
/// Windows 11's Notepad shows some of it cut short or garbled ("Testing G1
/// on" for "Testing G1 on M1", now and then a wrong character), sent at
/// once or a character at a time; other programs (a search box, a
/// browser) show all of it at any pace.
pub fn text(text: &str) -> std::io::Result<()> {
    send(&text_inputs(text))
}

fn text_inputs(text: &str) -> Vec<INPUT> {
    text.chars().flat_map(char_inputs).collect()
}

/// One character's key events: down and up, for each UTF-16 unit.
fn char_inputs(ch: char) -> Vec<INPUT> {
    let special = match ch {
        '\n' => from_usage(0x28),
        '\t' => from_usage(0x2B),
        '\r' => return Vec::new(),
        _ => None,
    };
    if let Some(k) = special {
        return vec![key_input(k, false), key_input(k, true)];
    }
    let mut units = [0u16; 2];
    let mut out = Vec::new();
    for &unit in ch.encode_utf16(&mut units).iter() {
        out.push(input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE));
        out.push(input(VIRTUAL_KEY(0), unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(i: &INPUT) -> KEYBD_EVENT_FLAGS {
        unsafe { i.Anonymous.ki.dwFlags }
    }
    fn scan(i: &INPUT) -> u16 {
        unsafe { i.Anonymous.ki.wScan }
    }

    #[test]
    fn extended_keys_carry_the_e0_flag() {
        let right_ctrl = key_input(key_named("right_ctrl").unwrap(), false);
        assert_eq!(flags(&right_ctrl), KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY);
        assert_eq!(scan(&right_ctrl), 0x1D);
        let left_ctrl = key_input(key_named("left_ctrl").unwrap(), true);
        assert_eq!(flags(&left_ctrl), KEYEVENTF_SCANCODE | KEYEVENTF_KEYUP);
    }

    #[test]
    fn text_outside_the_bmp_is_two_utf16_units() {
        // U+1F600: a surrogate pair, each unit pressed and released.
        let i = text_inputs("\u{1F600}");
        assert_eq!(i.len(), 4);
        assert_eq!(scan(&i[0]), 0xD83D);
        assert_eq!(scan(&i[2]), 0xDE00);
        assert!(i.iter().all(|x| flags(x).contains(KEYEVENTF_UNICODE)));
    }

    #[test]
    fn a_line_break_presses_enter() {
        let i = text_inputs("a\r\nb");
        assert_eq!(i.len(), 6);
        assert_eq!(scan(&i[2]), 0x1C);
        assert!(flags(&i[2]).contains(KEYEVENTF_SCANCODE));
    }

    #[test]
    fn names_reach_their_scan_codes() {
        assert_eq!(key_named("esc"), Some(Key::Scan { code: 0x01, extended: false }));
        assert_eq!(key_named("a"), Some(Key::Scan { code: 0x1E, extended: false }));
        assert_eq!(key_named("0"), Some(Key::Scan { code: 0x0B, extended: false }));
        assert_eq!(key_named("f12"), Some(Key::Scan { code: 0x58, extended: false }));
        assert_eq!(key_named("f15"), Some(Key::Scan { code: 0x66, extended: false }));
        assert_eq!(key_named("f24"), Some(Key::Scan { code: 0x76, extended: false }));
        assert_eq!(key_named("up"), Some(Key::Scan { code: 0x48, extended: true }));
        assert_eq!(key_named("pause"), Some(Key::Pause));
        assert_eq!(key_named("logo"), None);
    }

    #[test]
    fn every_keyboard_key_name_types_something() {
        for k in keymap::KEYS.iter().filter(|k| k.zone <= 111) {
            assert!(key_named(k.name).is_some(), "{}", k.name);
        }
    }

    #[test]
    fn modifier_bits_in_order() {
        let m = modifiers(0x01 | 0x40);
        assert_eq!(m, [key_named("left_ctrl").unwrap(), key_named("right_alt").unwrap()]);
    }
}
