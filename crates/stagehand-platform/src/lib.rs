//! Everything Stagehand needs from the operating system.
//!
//! Each module exposes the same functions on every supported system and picks its
//! implementation by target. Only Windows is implemented so far.

#[cfg(not(windows))]
compile_error!("Stagehand only supports Windows so far");

pub mod autostart;
pub mod clock;
pub mod package;
pub mod paths;
pub mod shortcut_recorder;
pub mod single_instance;
pub mod window_icon;
