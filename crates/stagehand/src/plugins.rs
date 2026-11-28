use slint::{ModelRc, SharedString, VecModel};
use stagehand_core::{Annotations, Key, Plugin, Shortcut};
use stagehand_laser_pointer::{Color, LaserPointer, LaserPointerSettings, MouseButton};
use stagehand_presenter_window::{PresenterWindow, PresenterWindowSettings};
use stagehand_taskbar_switch::{Placement, TaskbarSwitch, TaskbarSwitchSettings};

use crate::{
    LaserPointerOptions, PresenterWindowOptions, SettingsWindow, ShortcutOptions,
    TaskbarSwitchOptions,
};

/// The plugins compiled into this build, in the order the settings window lists them.
///
/// Plugins are linked in at build time, so adding one means adding its crate as a
/// dependency and constructing it here.
pub fn all() -> Vec<Box<dyn Plugin>> {
    // What the tools draw on the screen, which the Presenter Window repeats for viewers
    // who are shown a single window.
    let annotations = Annotations::new();
    vec![
        Box::new(LaserPointer::new(annotations.clone())),
        Box::new(PresenterWindow::new(annotations)),
        Box::new(TaskbarSwitch::new()),
    ]
}

/// Where `item` is in `list`, as the settings window counts.
fn position<T: PartialEq>(list: &[T], item: &T) -> i32 {
    list.iter()
        .position(|candidate| candidate == item)
        .unwrap_or_default() as i32
}

/// The item of `list` the settings window means by `index`.
fn at<T: Copy>(list: &[T], index: i32) -> Option<T> {
    usize::try_from(index)
        .ok()
        .and_then(|index| list.get(index).copied())
}

// The settings window shows a placement as its position in `Placement::ALL`; the list
// of names in the window is in the same order.

pub fn taskbar_switch_options(settings: &TaskbarSwitchSettings) -> TaskbarSwitchOptions {
    TaskbarSwitchOptions {
        keyboard_placement: position(&Placement::ALL, &settings.keyboard_placement),
        tablet_placement: position(&Placement::ALL, &settings.tablet_placement),
        apply_on_start: settings.apply_on_start,
    }
}

pub fn taskbar_switch_settings(options: &TaskbarSwitchOptions) -> TaskbarSwitchSettings {
    let defaults = TaskbarSwitchSettings::default();
    TaskbarSwitchSettings {
        keyboard_placement: at(&Placement::ALL, options.keyboard_placement)
            .unwrap_or(defaults.keyboard_placement),
        tablet_placement: at(&Placement::ALL, options.tablet_placement)
            .unwrap_or(defaults.tablet_placement),
        apply_on_start: options.apply_on_start,
    }
}

// The settings window shows a shortcut's key as its position in a list that starts with
// "None" and goes on with every key, and a mouse button as its position in
// `MouseButton::ALL`. The always-on button is "None" followed by `MouseButton::SPARE`.

/// The names the settings window lists for a shortcut's key.
pub fn shortcut_keys() -> ModelRc<SharedString> {
    let names: Vec<SharedString> = std::iter::once("None".into())
        .chain(Key::all().map(|key| key.name().into()))
        .collect();
    ModelRc::new(VecModel::from(names))
}

// The names the settings window knows its shortcuts by; app.slint uses the same ones.
pub const LASER_MOUSE_SHORTCUT: &str = "laser-pointer.mouse";
pub const LASER_PEN_SHORTCUT: &str = "laser-pointer.pen";
pub const PRESENTER_SHARE_SHORTCUT: &str = "presenter-window.share";
pub const PRESENTER_STOP_SHORTCUT: &str = "presenter-window.stop";

/// What `shortcut` is already used for in the settings window, as words that finish
/// "is already used to ...". A shortcut can only do one thing, so recording it a second
/// time would leave one of the two dead. `except` is the shortcut being recorded, which
/// may of course be given the keys it already has.
pub fn shortcut_user(
    window: &SettingsWindow,
    shortcut: Shortcut,
    except: &str,
) -> Option<&'static str> {
    let laser_pointer = window.get_laser_pointer();
    let presenter_window = window.get_presenter_window();
    [
        (
            LASER_MOUSE_SHORTCUT,
            laser_pointer.mouse_shortcut,
            "turn the laser on or off",
        ),
        (
            LASER_PEN_SHORTCUT,
            laser_pointer.pen_shortcut,
            "turn the laser on or off for the pen",
        ),
        (
            PRESENTER_SHARE_SHORTCUT,
            presenter_window.share_shortcut,
            "share a window",
        ),
        (
            PRESENTER_STOP_SHORTCUT,
            presenter_window.stop_shortcut,
            "stop sharing",
        ),
    ]
    .into_iter()
    .find(|(target, options, _)| *target != except && self::shortcut(options) == Some(shortcut))
    .map(|(_, _, user)| user)
}

pub fn shortcut_options(shortcut: Option<Shortcut>) -> ShortcutOptions {
    let Some(shortcut) = shortcut else {
        return ShortcutOptions::default();
    };
    let keys: Vec<Key> = Key::all().collect();
    ShortcutOptions {
        win: shortcut.win,
        ctrl: shortcut.ctrl,
        alt: shortcut.alt,
        shift: shortcut.shift,
        key: position(&keys, &shortcut.key) + 1,
    }
}

fn shortcut(options: &ShortcutOptions) -> Option<Shortcut> {
    let keys: Vec<Key> = Key::all().collect();
    Some(Shortcut {
        win: options.win,
        ctrl: options.ctrl,
        alt: options.alt,
        shift: options.shift,
        key: at(&keys, options.key.checked_sub(1)?)?,
    })
}

pub fn presenter_window_options(settings: &PresenterWindowSettings) -> PresenterWindowOptions {
    PresenterWindowOptions {
        share_shortcut: shortcut_options(settings.share_shortcut),
        stop_shortcut: shortcut_options(settings.stop_shortcut),
    }
}

pub fn presenter_window_settings(options: &PresenterWindowOptions) -> PresenterWindowSettings {
    PresenterWindowSettings {
        share_shortcut: shortcut(&options.share_shortcut),
        stop_shortcut: shortcut(&options.stop_shortcut),
    }
}

pub fn laser_pointer_options(settings: &LaserPointerSettings) -> LaserPointerOptions {
    let color = settings.color;
    LaserPointerOptions {
        mouse_shortcut: shortcut_options(settings.activation_shortcut),
        pen_shortcut: shortcut_options(settings.pen_activation_shortcut),
        activation_button: position(&MouseButton::ALL, &settings.activation_button),
        always_on_button: settings
            .always_on_button
            .map_or(0, |button| position(&MouseButton::SPARE, &button) + 1),
        suppress_activation_button: settings.suppress_activation_button,
        glow_enabled: settings.glow_enabled,
        pen_render_when_close: settings.pen_render_when_close,
        color: slint::Color::from_rgb_u8(color.red, color.green, color.blue),
        opacity_percent: (f32::from(color.alpha) * 100.0 / 255.0).round() as i32,
        size: settings.size as i32,
        decay_time_ms: settings.decay_time_ms as i32,
        decay_length: settings.decay_length as i32,
        streamline_percent: settings.streamline_percent as i32,
    }
}

pub fn laser_pointer_settings(options: &LaserPointerOptions) -> LaserPointerSettings {
    let defaults = LaserPointerSettings::default();
    let number =
        |value: i32, (low, high): (u32, u32)| u32::try_from(value).unwrap_or(low).clamp(low, high);
    let opacity = options.opacity_percent.clamp(0, 100) as f32;
    LaserPointerSettings {
        activation_shortcut: shortcut(&options.mouse_shortcut),
        pen_activation_shortcut: shortcut(&options.pen_shortcut),
        activation_button: at(&MouseButton::ALL, options.activation_button)
            .unwrap_or(defaults.activation_button),
        always_on_button: options
            .always_on_button
            .checked_sub(1)
            .and_then(|index| at(&MouseButton::SPARE, index)),
        suppress_activation_button: options.suppress_activation_button,
        glow_enabled: options.glow_enabled,
        pen_render_when_close: options.pen_render_when_close,
        color: Color {
            alpha: (opacity * 255.0 / 100.0).round() as u8,
            red: options.color.red(),
            green: options.color.green(),
            blue: options.color.blue(),
        },
        size: number(options.size, LaserPointerSettings::SIZE_RANGE),
        decay_time_ms: number(
            options.decay_time_ms,
            LaserPointerSettings::DECAY_TIME_MS_RANGE,
        ),
        decay_length: number(
            options.decay_length,
            LaserPointerSettings::DECAY_LENGTH_RANGE,
        ),
        streamline_percent: number(
            options.streamline_percent,
            LaserPointerSettings::STREAMLINE_PERCENT_RANGE,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_placement_survives_the_trip_through_the_settings_window() {
        for placement in Placement::ALL {
            let settings = TaskbarSwitchSettings {
                keyboard_placement: placement,
                tablet_placement: placement,
                apply_on_start: false,
            };

            let options = taskbar_switch_options(&settings);

            assert_eq!(taskbar_switch_settings(&options), settings);
        }
    }

    #[test]
    fn an_index_outside_the_list_falls_back_to_the_default() {
        let options = TaskbarSwitchOptions {
            keyboard_placement: -1,
            tablet_placement: 99,
            apply_on_start: true,
        };

        assert_eq!(
            taskbar_switch_settings(&options),
            TaskbarSwitchSettings::default()
        );
    }

    #[test]
    fn the_laser_draws_with_the_left_button_and_nothing_always_draws_by_default() {
        let options = laser_pointer_options(&LaserPointerSettings::default());

        // The first entry of each list in the settings window: "Left" and "None".
        assert_eq!(options.activation_button, 0);
        assert_eq!(options.always_on_button, 0);
    }

    #[test]
    fn the_always_draw_list_never_reaches_left_or_right() {
        for index in -1..=10 {
            let options = LaserPointerOptions {
                always_on_button: index,
                ..laser_pointer_options(&LaserPointerSettings::default())
            };

            let button = laser_pointer_settings(&options).always_on_button;

            assert!(button.is_none_or(MouseButton::is_spare), "{index}");
        }
    }

    #[test]
    fn laser_pointer_settings_survive_the_trip_through_the_settings_window() {
        let defaults = LaserPointerSettings::default();
        assert_eq!(
            laser_pointer_settings(&laser_pointer_options(&defaults)),
            defaults
        );

        let changed = LaserPointerSettings {
            activation_shortcut: None,
            pen_activation_shortcut: Shortcut::parse("Ctrl+Alt+F12"),
            activation_button: MouseButton::X2,
            always_on_button: Some(MouseButton::X1),
            suppress_activation_button: false,
            glow_enabled: false,
            pen_render_when_close: true,
            color: Color::parse("#800A84FF").unwrap(),
            size: 40,
            decay_time_ms: 2500,
            decay_length: 120,
            streamline_percent: 30,
        };
        assert_eq!(
            laser_pointer_settings(&laser_pointer_options(&changed)),
            changed
        );
    }

    #[test]
    fn every_key_and_button_has_its_own_place_in_the_settings_window() {
        for key in Key::all() {
            let shortcut = Shortcut {
                win: true,
                ctrl: false,
                alt: false,
                shift: false,
                key,
            };
            assert_eq!(
                self::shortcut(&shortcut_options(Some(shortcut))),
                Some(shortcut)
            );
        }
        assert_eq!(self::shortcut(&shortcut_options(None)), None);

        for button in MouseButton::SPARE {
            let settings = LaserPointerSettings {
                always_on_button: Some(button),
                ..LaserPointerSettings::default()
            };
            let options = laser_pointer_options(&settings);
            assert_eq!(
                laser_pointer_settings(&options).always_on_button,
                Some(button)
            );
        }
    }

    #[test]
    fn presenter_window_settings_survive_the_trip_through_the_settings_window() {
        for settings in [
            PresenterWindowSettings::default(),
            PresenterWindowSettings {
                share_shortcut: None,
                stop_shortcut: Shortcut::parse("Alt+F4"),
            },
        ] {
            let options = presenter_window_options(&settings);

            assert_eq!(presenter_window_settings(&options), settings);
        }
    }

    #[test]
    fn the_key_list_starts_with_none() {
        use slint::Model;

        let keys = shortcut_keys();

        assert_eq!(keys.row_count(), 1 + Key::all().count());
        assert_eq!(keys.row_data(0).unwrap(), "None");
        assert_eq!(keys.row_data(1).unwrap(), "A");
    }
}
