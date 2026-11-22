//! Deciding which window to mirror, and reading what is needed from it.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    GA_ROOT, GW_HWNDPREV, GetAncestor, GetClassNameW, GetCursorPos, GetForegroundWindow, GetWindow,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsIconic,
    IsWindow, IsWindowVisible, WindowFromPoint,
};

/// Surfaces of the shell, which are not applications. Sharing with the pointer over the
/// taskbar or the desktop falls back to the window in front instead.
const SHELL_CLASSES: [&str; 10] = [
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Progman",
    "WorkerW",
    // The tray overflow, on Windows 10 and on Windows 11.
    "NotifyIconOverflowWindow",
    "TopLevelWindowForOverflowXamlIsland",
    // Flyouts of the shell and of applications.
    "Xaml_WindowedPopupClass",
    // Task view and other islands of the shell.
    "XamlExplorerHostIslandWindow",
    // Quick settings.
    "ControlCenterWindow",
    "Windows.UI.Core.CoreWindow",
];

pub fn is_shell_class(class: &str) -> bool {
    SHELL_CLASSES.contains(&class)
}

fn dwm_attribute<T: Default>(
    window: HWND,
    attribute: windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE,
) -> Option<T> {
    let mut value = T::default();
    // SAFETY: `value` is a live local of exactly the size passed along with it.
    unsafe {
        DwmGetWindowAttribute(
            window,
            attribute,
            (&raw mut value).cast::<c_void>(),
            size_of::<T>() as u32,
        )
    }
    .ok()
    .map(|()| value)
}

/// A cloaked window is composed but not on screen: it is on another virtual desktop, or
/// belongs to a suspended app.
fn is_cloaked(window: HWND) -> bool {
    dwm_attribute::<u32>(window, DWMWA_CLOAKED).is_some_and(|cloaked| cloaked != 0)
}

/// Whether a window actually shows on screen. The window order is full of ones that do
/// not, and they hold places in it without covering anything.
pub fn is_on_screen(window: HWND) -> bool {
    // SAFETY: both calls only read the state of a window and cope with a stale handle.
    let shown = unsafe { IsWindowVisible(window).as_bool() && !IsIconic(window).as_bool() };
    shown && !is_cloaked(window)
}

pub fn is_alive(window: HWND) -> bool {
    // SAFETY: the call copes with a handle that is no longer a window.
    unsafe { IsWindow(Some(window)) }.as_bool()
}

pub fn window_rect(window: HWND) -> Option<RECT> {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid place for the rectangle.
    unsafe { GetWindowRect(window, &mut rect) }
        .ok()
        .map(|()| rect)
        .filter(|rect| rect.right > rect.left && rect.bottom > rect.top)
}

/// Where a window is drawn. Its window rectangle is a few pixels larger, because it
/// includes the invisible border Windows keeps around the frame for resizing, and it is
/// not what window capture uses either.
pub fn visible_bounds(window: HWND) -> Option<RECT> {
    dwm_attribute::<RECT>(window, DWMWA_EXTENDED_FRAME_BOUNDS)
        .filter(|rect| rect.right > rect.left && rect.bottom > rect.top)
        .or_else(|| window_rect(window))
}

pub fn title(window: HWND) -> String {
    // SAFETY: the buffer is as long as the length passed with it.
    unsafe {
        let length = GetWindowTextLengthW(window);
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let copied = GetWindowTextW(window, &mut buffer);
        String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
    }
}

fn class_name(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: the buffer is as long as the slice says.
    let copied = unsafe { GetClassNameW(window, &mut buffer) };
    String::from_utf16_lossy(&buffer[..copied.max(0) as usize])
}

pub fn is_own_window(window: HWND) -> bool {
    let mut process = 0u32;
    // SAFETY: `process` is a valid place for the process id.
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut process));
        process == GetCurrentProcessId()
    }
}

/// Only ordinary application windows are worth mirroring. The taskbar, the desktop,
/// windows that are not on screen and Stagehand's own windows are not.
pub fn is_presentable(window: HWND) -> bool {
    !window.is_invalid()
        && is_alive(window)
        && is_on_screen(window)
        && window_rect(window).is_some()
        && !title(window).is_empty()
        && !is_shell_class(&class_name(window))
        && !is_own_window(window)
}

pub fn foreground() -> Option<HWND> {
    // SAFETY: the call takes no arguments.
    let window = unsafe { GetForegroundWindow() };
    is_presentable(window).then_some(window)
}

/// The window the user means when pressing the shortcut. The one under the pointer wins
/// over the focused one: focus is invisible and usually still on whatever was clicked
/// last, such as the call, while the window being pointed at is the one meant.
pub fn under_pointer_or_foreground() -> Option<HWND> {
    let mut position = POINT::default();
    // SAFETY: `position` is a valid place for the position, and the other calls only
    // look windows up.
    let under = unsafe {
        GetCursorPos(&mut position)
            .ok()
            .map(|()| WindowFromPoint(position))
            // A control is turned into the top-level window that holds it.
            .map(|window| GetAncestor(window, GA_ROOT))
    };
    under
        .filter(|window| is_presentable(*window))
        .or_else(foreground)
}

/// The windows in front of `window` that show on screen, nearest first.
pub fn windows_above(window: HWND) -> impl Iterator<Item = HWND> {
    // SAFETY: walking the window order copes with windows that have gone away.
    let previous = |window: HWND| unsafe { GetWindow(window, GW_HWNDPREV) }.ok();
    std::iter::successors(previous(window), move |window| previous(*window))
        .filter(|window| is_on_screen(*window))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_surfaces_are_not_applications() {
        assert!(is_shell_class("Shell_TrayWnd"));
        assert!(is_shell_class("Progman"));
        assert!(!is_shell_class("Notepad"));
        assert!(!is_shell_class(""));
    }

    #[test]
    fn a_handle_that_is_not_a_window_is_not_presentable() {
        assert!(!is_presentable(HWND::default()));
        assert!(!is_presentable(HWND(0x7FFF_FFF0 as *mut _)));
    }
}
