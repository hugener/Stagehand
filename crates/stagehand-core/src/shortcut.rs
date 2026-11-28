use std::fmt;

/// The key of a keyboard shortcut, as a Windows virtual key code. Only letters, digits
/// and F1 to F24 can be made, which are the keys that mean the same on every layout.
/// Few keyboards have F13 to F24, but they are what the button of a pen and many
/// remapping tools send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key(u8);

impl Key {
    const F1: u8 = 0x70;
    const F24: u8 = 0x87;

    /// Every key, in the order a settings page lists them: A to Z, 0 to 9, F1 to F24.
    pub fn all() -> impl Iterator<Item = Self> {
        (b'A'..=b'Z')
            .chain(b'0'..=b'9')
            .chain(Self::F1..=Self::F24)
            .map(Self)
    }

    pub fn virtual_key(self) -> u32 {
        u32::from(self.0)
    }

    pub fn is_function_key(self) -> bool {
        (Self::F1..=Self::F24).contains(&self.0)
    }

    pub fn name(self) -> String {
        if self.is_function_key() {
            format!("F{}", self.0 - Self::F1 + 1)
        } else {
            char::from(self.0).to_string()
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::all().find(|key| key.name().eq_ignore_ascii_case(name))
    }
}

/// A keyboard shortcut. Stored as text such as `Win+Shift+L`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

impl Shortcut {
    /// Whether the shortcut can be registered without taking a key the user types with.
    /// A letter or digit on its own, or with only Shift, is ordinary typing.
    pub fn is_usable(&self) -> bool {
        self.win || self.ctrl || self.alt || self.key.is_function_key()
    }

    pub fn parse(text: &str) -> Option<Self> {
        let mut shortcut = Self {
            win: false,
            ctrl: false,
            alt: false,
            shift: false,
            key: Key(0),
        };
        let mut key = None;
        for part in text.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "win" => shortcut.win = true,
                "ctrl" => shortcut.ctrl = true,
                "alt" => shortcut.alt = true,
                "shift" => shortcut.shift = true,
                // A second key, or something that is not a key at all.
                _ if key.is_some() => return None,
                _ => key = Some(Key::from_name(part)?),
            }
        }
        shortcut.key = key?;
        Some(shortcut)
    }

    /// Reads a shortcut as it is kept in the settings file, where an empty text means
    /// "none" and anything missing or unreadable means `fallback`.
    pub fn from_stored(stored: Option<&str>, fallback: Option<Self>) -> Option<Self> {
        match stored {
            Some("") => None,
            Some(text) => Self::parse(text).or(fallback),
            None => fallback,
        }
    }

    /// The text to keep in the settings file for a shortcut that may be unset.
    pub fn to_stored(shortcut: Option<Self>) -> String {
        shortcut.map_or_else(String::new, |shortcut| shortcut.to_string())
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (held, name) in [
            (self.win, "Win"),
            (self.ctrl, "Ctrl"),
            (self.alt, "Alt"),
            (self.shift, "Shift"),
        ] {
            if held {
                write!(f, "{name}+")?;
            }
        }
        f.write_str(&self.key.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_read_whatever_the_case_and_order() {
        let shortcut = Shortcut::parse("shift + l + WIN").unwrap();

        assert!(shortcut.win && shortcut.shift && !shortcut.ctrl && !shortcut.alt);
        assert_eq!(shortcut.key.virtual_key(), u32::from(b'L'));
        assert_eq!(shortcut.to_string(), "Win+Shift+L");
        assert_eq!(Shortcut::parse("Ctrl+F12").unwrap().key.virtual_key(), 0x7B);
    }

    #[test]
    fn text_that_is_not_one_key_with_modifiers_is_not_a_shortcut() {
        for text in [
            "",
            "Win+Shift",
            "Ctrl+A+B",
            "Ctrl+Enter",
            "Ctrl++A",
            "F0",
            "F25",
        ] {
            assert_eq!(Shortcut::parse(text), None, "{text}");
        }
    }

    #[test]
    fn a_shortcut_that_is_ordinary_typing_is_not_usable() {
        assert!(!Shortcut::parse("L").unwrap().is_usable());
        assert!(!Shortcut::parse("Shift+7").unwrap().is_usable());
        assert!(Shortcut::parse("Alt+7").unwrap().is_usable());
        assert!(Shortcut::parse("F8").unwrap().is_usable());
    }

    #[test]
    fn every_key_survives_being_written_and_read_back() {
        let keys: Vec<Key> = Key::all().collect();

        assert_eq!(keys.len(), 26 + 10 + 24);
        for key in keys {
            assert_eq!(Key::from_name(&key.name()), Some(key));
        }
    }

    #[test]
    fn the_keys_a_pen_button_sends_are_shortcuts() {
        // On its own, as some pens send it, and with the Windows key, as a click, a
        // double click and a long press of a Windows Ink pen arrive.
        for (text, virtual_key, win) in [
            ("F24", 0x87, false),
            ("Win+F20", 0x83, true),
            ("Win+F19", 0x82, true),
            ("Win+F18", 0x81, true),
        ] {
            let shortcut = Shortcut::parse(text).unwrap();

            assert_eq!(shortcut.key.virtual_key(), virtual_key);
            assert_eq!(shortcut.win, win);
            assert!(shortcut.is_usable());
            assert_eq!(shortcut.to_string(), text);
        }
        // F1 is not mistaken for the start of F13.
        assert_eq!(Shortcut::parse("F1").unwrap().key.virtual_key(), 0x70);
        assert_eq!(Shortcut::parse("f13").unwrap().key.virtual_key(), 0x7C);
    }

    #[test]
    fn a_stored_shortcut_can_be_unset_or_fall_back() {
        let fallback = Shortcut::parse("Ctrl+F1");

        assert_eq!(Shortcut::from_stored(Some(""), fallback), None);
        assert_eq!(Shortcut::from_stored(None, fallback), fallback);
        assert_eq!(Shortcut::from_stored(Some("nonsense"), fallback), fallback);
        assert_eq!(
            Shortcut::from_stored(Some("Alt+2"), fallback),
            Shortcut::parse("Alt+2")
        );
        assert_eq!(Shortcut::to_stored(None), "");
        assert_eq!(Shortcut::to_stored(fallback), "Ctrl+F1");
    }
}
