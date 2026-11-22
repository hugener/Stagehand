//! Presenter Window: mirrors one window into a second one that can be shared in a call,
//! with what the other tools draw on the screen added on top.
//!
//! Sharing a single window in Teams or Zoom only shows that window's own content, so
//! anything drawn on an overlay above it, such as the laser pointer's trail, is invisible
//! to the viewers unless the whole screen is shared. The mirror is a window that holds
//! both: the picked window's picture and the drawing over it.

mod settings;
#[cfg(windows)]
mod windows;

pub use settings::PresenterWindowSettings;
#[cfg(windows)]
pub use windows::PresenterWindow;

/// The key the plugin's settings are stored under.
pub const ID: &str = "presenter-window";

/// Shares the window the user was last working in, or moves the mirror to it. For use
/// from another window, such as the tray menu, where the pointer says nothing about
/// which window is meant. See `Plugin::invoke`.
pub const ACTION_SHARE: &str = "share";

/// Stops sharing. See `Plugin::invoke`.
pub const ACTION_STOP: &str = "stop";
