use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An edge of the screen the taskbar can be docked on.
///
/// The numbers are the ones the Windows "Taskbar position" setting itself uses. Handing
/// that setting any other number crashes the process that does it, which is why an edge
/// only ever comes out of this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum TaskbarEdge {
    Bottom = 0,
    Top = 1,
    Left = 2,
    Right = 3,
}

impl TaskbarEdge {
    pub fn from_shell_value(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Bottom),
            1 => Some(Self::Top),
            2 => Some(Self::Left),
            3 => Some(Self::Right),
            _ => None,
        }
    }

    pub fn shell_value(self) -> i32 {
        self as i32
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

/// What the user picked for one posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    /// Leave the taskbar wherever it is.
    Unchanged,
    Bottom,
    Top,
    Left,
    Right,
}

impl Placement {
    /// Every choice, in the order a settings page lists them.
    pub const ALL: [Self; 5] = [
        Self::Unchanged,
        Self::Bottom,
        Self::Top,
        Self::Left,
        Self::Right,
    ];

    pub fn edge(self) -> Option<TaskbarEdge> {
        match self {
            Self::Unchanged => None,
            Self::Bottom => Some(TaskbarEdge::Bottom),
            Self::Top => Some(TaskbarEdge::Top),
            Self::Left => Some(TaskbarEdge::Left),
            Self::Right => Some(TaskbarEdge::Right),
        }
    }
}

/// How the user is currently working with the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Posture {
    /// A keyboard is attached and in use.
    Keyboard,
    /// The keyboard is detached or folded away.
    Tablet,
}

impl Posture {
    pub fn name(self) -> &'static str {
        match self {
            Self::Keyboard => "keyboard",
            Self::Tablet => "tablet",
        }
    }
}

/// Why the taskbar is being placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The plugin just started and reports the posture the device is already in.
    Started,
    PostureChanged,
    SettingsChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TaskbarSwitchSettings {
    pub keyboard_placement: Placement,
    pub tablet_placement: Placement,
    /// Place the taskbar for the current posture as soon as the plugin starts, instead
    /// of waiting for the next posture change.
    pub apply_on_start: bool,
}

impl Default for TaskbarSwitchSettings {
    fn default() -> Self {
        Self {
            keyboard_placement: Placement::Bottom,
            tablet_placement: Placement::Top,
            apply_on_start: true,
        }
    }
}

impl TaskbarSwitchSettings {
    /// Reads the settings as stored by the host. Each field that is missing or holds
    /// something unexpected falls back to its own default, so one bad value in a
    /// hand-edited file does not reset the others.
    pub fn from_value(value: &Value) -> Self {
        let defaults = Self::default();
        let field = |name: &str| value.get(name);
        let placement = |name: &str, fallback: Placement| {
            field(name)
                .and_then(|value| Placement::deserialize(value).ok())
                .unwrap_or(fallback)
        };
        Self {
            keyboard_placement: placement("keyboard_placement", defaults.keyboard_placement),
            tablet_placement: placement("tablet_placement", defaults.tablet_placement),
            apply_on_start: field("apply_on_start")
                .and_then(Value::as_bool)
                .unwrap_or(defaults.apply_on_start),
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("the settings only hold strings and a bool")
    }

    pub fn placement_for(&self, posture: Posture) -> Placement {
        match posture {
            Posture::Keyboard => self.keyboard_placement,
            Posture::Tablet => self.tablet_placement,
        }
    }

    /// The edge the taskbar should move to, or `None` when it should stay where it is.
    pub fn target_edge(&self, posture: Posture, trigger: Trigger) -> Option<TaskbarEdge> {
        if trigger == Trigger::Started && !self.apply_on_start {
            return None;
        }
        self.placement_for(posture).edge()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn no_stored_settings_gives_the_defaults() {
        let settings = TaskbarSwitchSettings::from_value(&Value::Null);

        assert_eq!(settings, TaskbarSwitchSettings::default());
        assert_eq!(settings.keyboard_placement, Placement::Bottom);
        assert_eq!(settings.tablet_placement, Placement::Top);
        assert!(settings.apply_on_start);
    }

    #[test]
    fn settings_round_trip_through_the_stored_form() {
        let settings = TaskbarSwitchSettings {
            keyboard_placement: Placement::Unchanged,
            tablet_placement: Placement::Left,
            apply_on_start: false,
        };

        let stored = settings.to_value();

        assert_eq!(
            stored,
            json!({
                "keyboard_placement": "unchanged",
                "tablet_placement": "left",
                "apply_on_start": false,
            })
        );
        assert_eq!(TaskbarSwitchSettings::from_value(&stored), settings);
    }

    #[test]
    fn a_bad_field_falls_back_without_resetting_the_others() {
        let stored = json!({
            "keyboard_placement": "diagonal",
            "tablet_placement": "right",
            "apply_on_start": "yes",
        });

        let settings = TaskbarSwitchSettings::from_value(&stored);

        assert_eq!(settings.keyboard_placement, Placement::Bottom);
        assert_eq!(settings.tablet_placement, Placement::Right);
        assert!(settings.apply_on_start);
    }

    #[test]
    fn each_posture_gets_its_own_placement() {
        let settings = TaskbarSwitchSettings {
            keyboard_placement: Placement::Bottom,
            tablet_placement: Placement::Left,
            apply_on_start: true,
        };

        assert_eq!(
            settings.target_edge(Posture::Keyboard, Trigger::PostureChanged),
            Some(TaskbarEdge::Bottom)
        );
        assert_eq!(
            settings.target_edge(Posture::Tablet, Trigger::PostureChanged),
            Some(TaskbarEdge::Left)
        );
    }

    #[test]
    fn unchanged_never_moves_the_taskbar() {
        let settings = TaskbarSwitchSettings {
            keyboard_placement: Placement::Unchanged,
            ..TaskbarSwitchSettings::default()
        };

        for trigger in [
            Trigger::Started,
            Trigger::PostureChanged,
            Trigger::SettingsChanged,
        ] {
            assert_eq!(settings.target_edge(Posture::Keyboard, trigger), None);
        }
    }

    #[test]
    fn starting_only_moves_the_taskbar_when_asked_to() {
        let settings = TaskbarSwitchSettings {
            apply_on_start: false,
            ..TaskbarSwitchSettings::default()
        };

        assert_eq!(
            settings.target_edge(Posture::Tablet, Trigger::Started),
            None
        );
        assert_eq!(
            settings.target_edge(Posture::Tablet, Trigger::PostureChanged),
            Some(TaskbarEdge::Top)
        );
        assert_eq!(
            settings.target_edge(Posture::Tablet, Trigger::SettingsChanged),
            Some(TaskbarEdge::Top)
        );
    }

    #[test]
    fn shell_values_outside_the_four_edges_are_rejected() {
        for edge in [
            TaskbarEdge::Bottom,
            TaskbarEdge::Top,
            TaskbarEdge::Left,
            TaskbarEdge::Right,
        ] {
            assert_eq!(
                TaskbarEdge::from_shell_value(edge.shell_value()),
                Some(edge)
            );
        }
        assert_eq!(TaskbarEdge::from_shell_value(-1), None);
        assert_eq!(TaskbarEdge::from_shell_value(4), None);
    }
}
