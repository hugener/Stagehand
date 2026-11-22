//! Taskbar Switch: moves the Windows taskbar to the edge the user picked for the
//! posture the device is in, one while the keyboard is in use and another as a tablet.
//!
//! The settings are available on every platform, so that a settings page can be built
//! against them; the plugin itself only exists on Windows.

mod settings;

#[cfg(windows)]
mod placement;
#[cfg(windows)]
mod plugin;
#[cfg(windows)]
mod posture;

pub use settings::{Placement, Posture, TaskbarEdge, TaskbarSwitchSettings, Trigger};

#[cfg(windows)]
pub use plugin::TaskbarSwitch;

/// Key of the plugin in the settings file.
pub const ID: &str = "taskbar-switch";
