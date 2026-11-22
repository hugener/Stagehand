use serde_json::{Value, json};
use stagehand_core::Shortcut;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresenterWindowSettings {
    /// Mirrors the window under the pointer, or moves the mirror to it when something
    /// is already mirrored. Moving keeps the mirror window itself, so a call that is
    /// already sharing it carries on.
    pub share_shortcut: Option<Shortcut>,
    /// Stops mirroring. Does nothing when nothing is mirrored, so it is harmless to
    /// press at any time.
    pub stop_shortcut: Option<Shortcut>,
}

impl Default for PresenterWindowSettings {
    fn default() -> Self {
        Self {
            share_shortcut: Shortcut::parse("Win+Ctrl+Shift+W"),
            stop_shortcut: Shortcut::parse("Win+Ctrl+Shift+Q"),
        }
    }
}

impl PresenterWindowSettings {
    /// Reads the settings as stored by the host. A shortcut that is missing or cannot
    /// be read falls back to its default; an empty one means "none".
    pub fn from_value(value: &Value) -> Self {
        let defaults = Self::default();
        let shortcut = |name: &str, fallback: Option<Shortcut>| {
            Shortcut::from_stored(value.get(name).and_then(Value::as_str), fallback)
        };
        Self {
            share_shortcut: shortcut("share_shortcut", defaults.share_shortcut),
            stop_shortcut: shortcut("stop_shortcut", defaults.stop_shortcut),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "share_shortcut": Shortcut::to_stored(self.share_shortcut),
            "stop_shortcut": Shortcut::to_stored(self.stop_shortcut),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_stored_settings_gives_the_defaults() {
        let settings = PresenterWindowSettings::from_value(&Value::Null);

        assert_eq!(settings, PresenterWindowSettings::default());
        assert_eq!(
            Shortcut::to_stored(settings.share_shortcut),
            "Win+Ctrl+Shift+W"
        );
        assert_eq!(
            Shortcut::to_stored(settings.stop_shortcut),
            "Win+Ctrl+Shift+Q"
        );
    }

    #[test]
    fn settings_round_trip_through_the_stored_form() {
        let settings = PresenterWindowSettings {
            share_shortcut: Shortcut::parse("Ctrl+Alt+F5"),
            stop_shortcut: None,
        };

        let stored = settings.to_value();

        assert_eq!(
            stored,
            json!({ "share_shortcut": "Ctrl+Alt+F5", "stop_shortcut": "" })
        );
        assert_eq!(PresenterWindowSettings::from_value(&stored), settings);
    }

    #[test]
    fn a_bad_shortcut_falls_back_without_resetting_the_other() {
        let stored = json!({ "share_shortcut": 7, "stop_shortcut": "Alt+9" });

        let settings = PresenterWindowSettings::from_value(&stored);

        assert_eq!(
            settings.share_shortcut,
            PresenterWindowSettings::default().share_shortcut
        );
        assert_eq!(settings.stop_shortcut, Shortcut::parse("Alt+9"));
    }
}
