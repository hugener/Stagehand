use std::fmt;

use serde_json::{Value, json};
use stagehand_core::Shortcut;

/// A physical mouse button.
///
/// Windows only ever reports five: mice that advertise more either have the extra ones
/// remapped to keystrokes by their vendor's software or never deliver them as mouse
/// input at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// The first side button, "Back" in most browsers.
    X1,
    /// The second side button, "Forward" in most browsers.
    X2,
}

impl MouseButton {
    /// Every button, in the order a settings page lists them.
    pub const ALL: [Self; 5] = [Self::Left, Self::Right, Self::Middle, Self::X1, Self::X2];

    /// The buttons that may be bound permanently, and swallowed while they are. Left and
    /// Right are left out: taking either away for good would make the machine unusable.
    pub const SPARE: [Self; 3] = [Self::Middle, Self::X1, Self::X2];

    pub fn is_spare(self) -> bool {
        Self::SPARE.contains(&self)
    }

    fn stored_name(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Middle => "middle",
            Self::X1 => "x1",
            Self::X2 => "x2",
        }
    }

    fn from_stored_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|button| button.stored_name() == name)
    }
}

/// The colour of the trail, opacity included. Stored as `#AARRGGBB`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub alpha: u8,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Color {
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if !digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        let [alpha, red, green, blue] = value.to_be_bytes();
        match digits.len() {
            8 => Some(Self {
                alpha,
                red,
                green,
                blue,
            }),
            // Without an opacity the colour is solid.
            6 => Some(Self {
                alpha: u8::MAX,
                red,
                green,
                blue,
            }),
            _ => None,
        }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "#{:02X}{:02X}{:02X}{:02X}",
            self.alpha, self.red, self.green, self.blue
        )
    }
}

/// The two shortcuts arm the mouse and the pen separately, so either can be used on its
/// own. Arming draws nothing by itself: the trail is only drawn while the activation
/// button or the pen tip is held down, and fades out once it is released.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaserPointerSettings {
    /// Arms and disarms the mouse.
    pub activation_shortcut: Option<Shortcut>,
    /// Arms and disarms the pen. Leaving it unset is how pen support is turned off.
    pub pen_activation_shortcut: Option<Shortcut>,
    /// Draws while held, but only while the mouse is armed. Any button is allowed
    /// because arming is deliberate and temporary.
    pub activation_button: MouseButton,
    /// Draws whenever it is held, with no shortcut needed. Only a spare button can be
    /// used for this.
    pub always_on_button: Option<MouseButton>,
    /// Keep the always-on button from also reaching the application underneath. Side
    /// buttons go back and forward in browsers, and a middle click opens links in tabs.
    /// The button that draws while the mouse is armed is kept back whatever this says.
    pub suppress_activation_button: bool,
    pub glow_enabled: bool,
    /// Draw as soon as the pen is close enough for the screen to sense it, instead of
    /// only while the tip is touching.
    pub pen_render_when_close: bool,
    pub color: Color,
    pub size: u32,
    pub decay_time_ms: u32,
    pub decay_length: u32,
    pub streamline_percent: u32,
}

impl LaserPointerSettings {
    pub const SIZE_RANGE: (u32, u32) = (1, 200);
    pub const DECAY_TIME_MS_RANGE: (u32, u32) = (100, 10_000);
    pub const DECAY_LENGTH_RANGE: (u32, u32) = (2, 500);
    pub const STREAMLINE_PERCENT_RANGE: (u32, u32) = (0, 95);
}

impl Default for LaserPointerSettings {
    fn default() -> Self {
        Self {
            activation_shortcut: Shortcut::parse("Win+Shift+L"),
            pen_activation_shortcut: None,
            activation_button: MouseButton::Left,
            always_on_button: None,
            suppress_activation_button: true,
            glow_enabled: true,
            pen_render_when_close: false,
            color: Color {
                alpha: 204,
                red: 255,
                green: 45,
                blue: 45,
            },
            size: 6,
            decay_time_ms: 1000,
            decay_length: 50,
            streamline_percent: 75,
        }
    }
}

impl LaserPointerSettings {
    /// Reads the settings as stored by the host. Each field that is missing or holds
    /// something unexpected falls back to its own default, and numbers are brought into
    /// range, so one bad value in a hand-edited file does not reset the others.
    pub fn from_value(value: &Value) -> Self {
        let defaults = Self::default();
        let text = |name: &str| value.get(name).and_then(Value::as_str);
        let flag = |name: &str, fallback: bool| {
            value.get(name).and_then(Value::as_bool).unwrap_or(fallback)
        };
        let number = |name: &str, fallback: u32, (low, high): (u32, u32)| {
            value
                .get(name)
                .and_then(Value::as_u64)
                .map_or(fallback, |number| {
                    number.clamp(low.into(), high.into()) as u32
                })
        };
        // An empty shortcut means "none"; anything unreadable means "the default".
        let shortcut = |name: &str, fallback: Option<Shortcut>| match text(name) {
            Some("") => None,
            Some(text) => Shortcut::parse(text).or(fallback),
            None => fallback,
        };

        Self {
            activation_shortcut: shortcut("activation_shortcut", defaults.activation_shortcut),
            pen_activation_shortcut: shortcut(
                "pen_activation_shortcut",
                defaults.pen_activation_shortcut,
            ),
            activation_button: text("activation_button")
                .and_then(MouseButton::from_stored_name)
                .unwrap_or(defaults.activation_button),
            always_on_button: text("always_on_button")
                .and_then(MouseButton::from_stored_name)
                .filter(|button| button.is_spare()),
            suppress_activation_button: flag(
                "suppress_activation_button",
                defaults.suppress_activation_button,
            ),
            glow_enabled: flag("glow_enabled", defaults.glow_enabled),
            pen_render_when_close: flag("pen_render_when_close", defaults.pen_render_when_close),
            color: text("color")
                .and_then(Color::parse)
                .unwrap_or(defaults.color),
            size: number("size", defaults.size, Self::SIZE_RANGE),
            decay_time_ms: number(
                "decay_time_ms",
                defaults.decay_time_ms,
                Self::DECAY_TIME_MS_RANGE,
            ),
            decay_length: number(
                "decay_length",
                defaults.decay_length,
                Self::DECAY_LENGTH_RANGE,
            ),
            streamline_percent: number(
                "streamline_percent",
                defaults.streamline_percent,
                Self::STREAMLINE_PERCENT_RANGE,
            ),
        }
    }

    pub fn to_value(&self) -> Value {
        let shortcut = |shortcut: Option<Shortcut>| {
            shortcut.map_or_else(String::new, |shortcut| shortcut.to_string())
        };
        json!({
            "activation_shortcut": shortcut(self.activation_shortcut),
            "pen_activation_shortcut": shortcut(self.pen_activation_shortcut),
            "activation_button": self.activation_button.stored_name(),
            "always_on_button": self.always_on_button.map_or("none", MouseButton::stored_name),
            "suppress_activation_button": self.suppress_activation_button,
            "glow_enabled": self.glow_enabled,
            "pen_render_when_close": self.pen_render_when_close,
            "color": self.color.to_string(),
            "size": self.size,
            "decay_time_ms": self.decay_time_ms,
            "decay_length": self.decay_length,
            "streamline_percent": self.streamline_percent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stored_settings_gives_the_defaults() {
        let settings = LaserPointerSettings::from_value(&Value::Null);

        assert_eq!(settings, LaserPointerSettings::default());
        assert_eq!(
            settings.activation_shortcut.unwrap().to_string(),
            "Win+Shift+L"
        );
        assert_eq!(settings.pen_activation_shortcut, None);
        assert_eq!(settings.activation_button, MouseButton::Left);
        assert_eq!(settings.always_on_button, None);
        assert_eq!(settings.color.to_string(), "#CCFF2D2D");
    }

    #[test]
    fn settings_round_trip_through_the_stored_form() {
        let settings = LaserPointerSettings {
            activation_shortcut: None,
            pen_activation_shortcut: Shortcut::parse("Ctrl+Alt+F9"),
            activation_button: MouseButton::Right,
            always_on_button: Some(MouseButton::X2),
            suppress_activation_button: false,
            glow_enabled: false,
            pen_render_when_close: true,
            color: Color::parse("#8000FF80").unwrap(),
            size: 24,
            decay_time_ms: 2500,
            decay_length: 120,
            streamline_percent: 30,
        };

        let stored = settings.to_value();

        assert_eq!(stored["activation_shortcut"], "");
        assert_eq!(stored["pen_activation_shortcut"], "Ctrl+Alt+F9");
        assert_eq!(stored["always_on_button"], "x2");
        assert_eq!(LaserPointerSettings::from_value(&stored), settings);
    }

    #[test]
    fn a_bad_field_falls_back_without_resetting_the_others() {
        let stored = json!({
            "activation_shortcut": "Win+Shift",
            "activation_button": "thumb",
            "color": "red",
            "size": "big",
            "decay_time_ms": 400,
        });

        let settings = LaserPointerSettings::from_value(&stored);
        let defaults = LaserPointerSettings::default();

        assert_eq!(settings.activation_shortcut, defaults.activation_shortcut);
        assert_eq!(settings.activation_button, defaults.activation_button);
        assert_eq!(settings.color, defaults.color);
        assert_eq!(settings.size, defaults.size);
        assert_eq!(settings.decay_time_ms, 400);
    }

    #[test]
    fn numbers_are_brought_into_range() {
        let stored = json!({
            "size": 0,
            "decay_time_ms": 999_999,
            "decay_length": 1,
            "streamline_percent": 100,
        });

        let settings = LaserPointerSettings::from_value(&stored);

        assert_eq!(settings.size, 1);
        assert_eq!(settings.decay_time_ms, 10_000);
        assert_eq!(settings.decay_length, 2);
        assert_eq!(settings.streamline_percent, 95);
    }

    #[test]
    fn left_and_right_cannot_be_always_on() {
        for (stored, expected) in [
            ("left", None),
            ("right", None),
            ("none", None),
            ("middle", Some(MouseButton::Middle)),
            ("x1", Some(MouseButton::X1)),
        ] {
            let settings = LaserPointerSettings::from_value(&json!({ "always_on_button": stored }));

            assert_eq!(settings.always_on_button, expected, "{stored}");
        }
    }

    #[test]
    fn colours_are_read_with_and_without_opacity() {
        let with_opacity = Color::parse("#ccff2d2d").unwrap();
        assert_eq!(
            (
                with_opacity.alpha,
                with_opacity.red,
                with_opacity.green,
                with_opacity.blue
            ),
            (204, 255, 45, 45)
        );
        assert_eq!(Color::parse("#00FF00").unwrap().alpha, 255);

        for text in ["", "CCFF2D2D", "#FFF", "#GGFF2D2D", "#+0FF2D2D"] {
            assert_eq!(Color::parse(text), None, "{text}");
        }
    }
}
