//! Names for keyboard scan codes (set 1, as Windows reports them in Raw Input
//! and takes them in `SendInput`), and which keys produce text.
//!
//! The text-producing keys matter for privacy: the key-timing logger refers to
//! them only by an anonymous id, so its logs can never be read back as what
//! was typed (passwords included). Keys that produce no text (modifiers,
//! F-keys, navigation) are named, because "Left Ctrl stuck" is the kind of
//! fact the logs are for.

/// A physical key as Raw Input identifies it: the set-1 make code plus the
/// E0 and E1 prefix flags (E0 tells e.g. Right Ctrl from Left Ctrl).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScanCode {
    pub code: u16,
    pub e0: bool,
    pub e1: bool,
}

impl ScanCode {
    /// Does this key type a character (letters, digits, punctuation, space,
    /// the numpad digits)? Those are the keys whose identity is withheld
    /// from logs.
    pub fn is_text(self) -> bool {
        if self.e1 {
            return false;
        }
        if self.e0 {
            // Numpad divide (E0 35) types '/'.
            return self.code == 0x35;
        }
        matches!(self.code,
            0x02..=0x0D      // 1 2 3 ... 0 + ´  (number row)
            | 0x10..=0x1B    // Q ... P Å ¨
            | 0x1E..=0x29    // A ... L Ø Æ, and § (0x29)
            | 0x2B..=0x35    // ' Z ... - (and the ANSI backslash at 0x2B)
            | 0x37           // numpad *
            | 0x39           // space
            | 0x47..=0x53    // numpad 7 8 9 - 4 5 6 + 1 2 3 0 ,
            | 0x56           // the ISO key left of Z (< >)
            | 0x59           // numpad = (some layouts)
            | 0x73 | 0x7D | 0x7E // JIS / Brazilian extra punctuation keys
        )
    }

    /// Name of a key that types nothing; `None` for text keys and for codes
    /// not in the table.
    pub fn name(self) -> Option<&'static str> {
        if self.is_text() {
            return None;
        }
        let n = match (self.e1, self.e0, self.code) {
            (true, _, 0x1D) => "Pause",
            (_, false, 0x01) => "Esc",
            (_, false, 0x0E) => "Backspace",
            (_, false, 0x0F) => "Tab",
            (_, false, 0x1C) => "Enter",
            (_, false, 0x1D) => "LCtrl",
            (_, false, 0x2A) => "LShift",
            (_, false, 0x36) => "RShift",
            (_, false, 0x38) => "LAlt",
            (_, false, 0x3A) => "CapsLock",
            (_, false, 0x3B) => "F1",
            (_, false, 0x3C) => "F2",
            (_, false, 0x3D) => "F3",
            (_, false, 0x3E) => "F4",
            (_, false, 0x3F) => "F5",
            (_, false, 0x40) => "F6",
            (_, false, 0x41) => "F7",
            (_, false, 0x42) => "F8",
            (_, false, 0x43) => "F9",
            (_, false, 0x44) => "F10",
            (_, false, 0x45) => "NumLock",
            (_, false, 0x46) => "ScrollLock",
            (_, false, 0x57) => "F11",
            (_, false, 0x58) => "F12",
            (_, false, 0x64) => "F13",
            (_, false, 0x65) => "F14",
            (_, false, 0x66) => "F15",
            (_, false, 0x67) => "F16",
            (_, false, 0x68) => "F17",
            (_, false, 0x69) => "F18",
            (_, false, 0x6A) => "F19",
            (_, false, 0x6B) => "F20",
            (_, false, 0x6C) => "F21",
            (_, false, 0x6D) => "F22",
            (_, false, 0x6E) => "F23",
            (_, false, 0x76) => "F24",
            (_, true, 0x1C) => "NumpadEnter",
            (_, true, 0x1D) => "RCtrl",
            (_, true, 0x37) => "PrintScreen",
            (_, true, 0x38) => "AltGr",
            (_, true, 0x47) => "Home",
            (_, true, 0x48) => "Up",
            (_, true, 0x49) => "PageUp",
            (_, true, 0x4B) => "Left",
            (_, true, 0x4D) => "Right",
            (_, true, 0x4F) => "End",
            (_, true, 0x50) => "Down",
            (_, true, 0x51) => "PageDown",
            (_, true, 0x52) => "Insert",
            (_, true, 0x53) => "Delete",
            (_, true, 0x5B) => "LWin",
            (_, true, 0x5C) => "RWin",
            (_, true, 0x5D) => "Menu",
            _ => return None,
        };
        Some(n)
    }

    /// Keys people routinely hold down on purpose so they auto-repeat:
    /// Backspace, Delete, the arrows and the navigation block, and the
    /// modifiers (held, not repeated, but held for long). A repeating text key
    /// is unusual in normal typing; a repeating one of these is not.
    pub fn is_usually_held(self) -> bool {
        matches!(
            self.name(),
            Some(
                "Backspace"
                    | "Delete"
                    | "Up"
                    | "Down"
                    | "Left"
                    | "Right"
                    | "PageUp"
                    | "PageDown"
                    | "Home"
                    | "End"
                    | "LCtrl"
                    | "RCtrl"
                    | "LShift"
                    | "RShift"
                    | "LAlt"
                    | "AltGr"
                    | "LWin"
                    | "RWin"
                    | "Tab"
                    | "Enter"
                    | "Esc"
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sc(code: u16, e0: bool) -> ScanCode {
        ScanCode { code, e0, e1: false }
    }

    #[test]
    fn letters_digits_and_space_are_text_and_have_no_name() {
        for code in [0x10, 0x1E, 0x2C, 0x02, 0x0B, 0x39, 0x56, 0x29, 0x52] {
            assert!(sc(code, false).is_text(), "{code:#x}");
            assert_eq!(sc(code, false).name(), None, "{code:#x}");
        }
    }

    #[test]
    fn the_e0_twin_of_a_numpad_key_is_navigation_not_text() {
        // 0x48 is numpad 8 without E0 and the Up arrow with it.
        assert!(sc(0x48, false).is_text());
        assert_eq!(sc(0x48, true).name(), Some("Up"));
        assert!(!sc(0x48, true).is_text());
    }

    #[test]
    fn modifiers_and_function_keys_are_named() {
        assert_eq!(sc(0x1D, false).name(), Some("LCtrl"));
        assert_eq!(sc(0x1D, true).name(), Some("RCtrl"));
        assert_eq!(sc(0x66, false).name(), Some("F15"));
        assert_eq!(sc(0x65, false).name(), Some("F14"));
        assert_eq!(ScanCode { code: 0x1D, e0: false, e1: true }.name(), Some("Pause"));
    }
}
