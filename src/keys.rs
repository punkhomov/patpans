use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub name: &'static str,
    pub linux_code: u16,
    pub windows_vk: u16,
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name)
    }
}

macro_rules! key_table {
    ($(($name:literal, $linux:expr, $vk:expr)),+ $(,)?) => {
        pub const KEYS: &[Key] = &[
            $(Key { name: $name, linux_code: $linux, windows_vk: $vk }),+
        ];
    };
}

key_table!(
    ("Escape", 1, 0x1B),
    ("1", 2, b'1' as u16),
    ("2", 3, b'2' as u16),
    ("3", 4, b'3' as u16),
    ("4", 5, b'4' as u16),
    ("5", 6, b'5' as u16),
    ("6", 7, b'6' as u16),
    ("7", 8, b'7' as u16),
    ("8", 9, b'8' as u16),
    ("9", 10, b'9' as u16),
    ("0", 11, b'0' as u16),
    ("Minus", 12, 0xBD),
    ("Equal", 13, 0xBB),
    ("Backspace", 14, 0x08),
    ("Tab", 15, 0x09),
    ("Q", 16, b'Q' as u16),
    ("W", 17, b'W' as u16),
    ("E", 18, b'E' as u16),
    ("R", 19, b'R' as u16),
    ("T", 20, b'T' as u16),
    ("Y", 21, b'Y' as u16),
    ("U", 22, b'U' as u16),
    ("I", 23, b'I' as u16),
    ("O", 24, b'O' as u16),
    ("P", 25, b'P' as u16),
    ("LeftBracket", 26, 0xDB),
    ("RightBracket", 27, 0xDD),
    ("Enter", 28, 0x0D),
    ("LeftCtrl", 29, 0xA2),
    ("A", 30, b'A' as u16),
    ("S", 31, b'S' as u16),
    ("D", 32, b'D' as u16),
    ("F", 33, b'F' as u16),
    ("G", 34, b'G' as u16),
    ("H", 35, b'H' as u16),
    ("J", 36, b'J' as u16),
    ("K", 37, b'K' as u16),
    ("L", 38, b'L' as u16),
    ("Semicolon", 39, 0xBA),
    ("Apostrophe", 40, 0xDE),
    ("Grave", 41, 0xC0),
    ("LeftShift", 42, 0xA0),
    ("Backslash", 43, 0xDC),
    ("Z", 44, b'Z' as u16),
    ("X", 45, b'X' as u16),
    ("C", 46, b'C' as u16),
    ("V", 47, b'V' as u16),
    ("B", 48, b'B' as u16),
    ("N", 49, b'N' as u16),
    ("M", 50, b'M' as u16),
    ("Comma", 51, 0xBC),
    ("Period", 52, 0xBE),
    ("Slash", 53, 0xBF),
    ("RightShift", 54, 0xA1),
    ("LeftAlt", 56, 0xA4),
    ("Space", 57, 0x20),
    ("CapsLock", 58, 0x14),
    ("F1", 59, 0x70),
    ("F2", 60, 0x71),
    ("F3", 61, 0x72),
    ("F4", 62, 0x73),
    ("F5", 63, 0x74),
    ("F6", 64, 0x75),
    ("F7", 65, 0x76),
    ("F8", 66, 0x77),
    ("F9", 67, 0x78),
    ("F10", 68, 0x79),
    ("NumLock", 69, 0x90),
    ("ScrollLock", 70, 0x91),
    ("F11", 87, 0x7A),
    ("F12", 88, 0x7B),
    ("RightCtrl", 97, 0xA3),
    ("RightAlt", 100, 0xA5),
    ("Home", 102, 0x24),
    ("Up", 103, 0x26),
    ("PageUp", 104, 0x21),
    ("Left", 105, 0x25),
    ("Right", 106, 0x27),
    ("End", 107, 0x23),
    ("Down", 108, 0x28),
    ("PageDown", 109, 0x22),
    ("Insert", 110, 0x2D),
    ("Delete", 111, 0x2E),
    ("LeftMeta", 125, 0x5B),
    ("RightMeta", 126, 0x5C),
);

fn alias(name: &str) -> Option<&'static str> {
    let canonical = match name.to_ascii_lowercase().as_str() {
        "esc" => "Escape",
        "return" => "Enter",
        "ctrl" | "control" => "LeftCtrl",
        "shift" => "LeftShift",
        "alt" => "LeftAlt",
        "win" | "meta" | "super" | "cmd" => "LeftMeta",
        "spacebar" => "Space",
        "del" => "Delete",
        "ins" => "Insert",
        "arrowleft" => "Left",
        "arrowright" => "Right",
        "arrowup" => "Up",
        "arrowdown" => "Down",
        _ => return None,
    };
    Some(canonical)
}

pub fn by_name(name: &str) -> Option<Key> {
    let trimmed = name.trim();
    if let Some(canonical) = alias(trimmed) {
        return by_name(canonical);
    }
    KEYS.iter()
        .copied()
        .find(|key| key.name.eq_ignore_ascii_case(trimmed))
}

pub fn by_linux_code(code: u16) -> Option<Key> {
    KEYS.iter().copied().find(|key| key.linux_code == code)
}

pub fn by_windows_vk(vk: u16) -> Option<Key> {
    KEYS.iter().copied().find(|key| key.windows_vk == vk)
}

pub const fn unknown_linux(code: u16) -> Key {
    Key {
        name: "<unknown>",
        linux_code: code,
        windows_vk: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_case_insensitive_and_trimmed() {
        assert_eq!(by_name(" a ").unwrap().name, "A");
        assert_eq!(by_name("f8").unwrap().name, "F8");
    }

    #[test]
    fn aliases_resolve_to_canonical_keys() {
        assert_eq!(by_name("ctrl").unwrap().name, "LeftCtrl");
        assert_eq!(by_name("Esc").unwrap().name, "Escape");
        assert_eq!(by_name("ArrowLeft").unwrap().name, "Left");
    }

    #[test]
    fn codes_round_trip() {
        for key in KEYS {
            assert_eq!(by_linux_code(key.linux_code).unwrap().name, key.name);
            assert_eq!(by_windows_vk(key.windows_vk).unwrap().name, key.name);
        }
    }
}
