//! Laser Pointer: draws a fading trail on screen with the mouse or a pen.
//!
//! The trail model and the settings have no operating system dependencies, so they are
//! shared by every platform's implementation.

mod settings;
pub mod stroke;
#[cfg(windows)]
mod windows;

pub use settings::{Color, LaserPointerSettings, MouseButton};
#[cfg(windows)]
pub use windows::LaserPointer;

/// The key the plugin's settings are stored under.
pub const ID: &str = "laser-pointer";
