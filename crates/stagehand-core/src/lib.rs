//! The parts of Stagehand that do not depend on an operating system or a UI toolkit:
//! the contract every plugin implements, the host that runs plugins, the settings file
//! they are configured from, and the few things plugins have in common.

mod annotations;
mod plugin;
mod settings;
mod shortcut;

pub use annotations::{Annotations, Shape};
pub use plugin::{HostError, Plugin, PluginError, PluginHost, PluginInfo, PluginStatus};
pub use settings::{PluginSettings, Settings, SettingsStore};
pub use shortcut::{Key, Shortcut};
