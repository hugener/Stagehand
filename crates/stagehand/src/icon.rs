//! Stagehand's icon where the app has to supply it itself.
//!
//! The files are made from the drawings in the `assets` folder at the top of the
//! repository by `assets\make-icons.ps1`.

use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::window::Window;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};
use stagehand_platform::window_icon;

const TRAY_SIZE: u32 = 32;
/// Bare pixels, four bytes each: red, green, blue and opacity. That way the app does
/// not have to carry an image decoder for one small picture.
const TRAY_PIXELS: &[u8] = include_bytes!("../assets/tray.rgba");

/// The icon in the notification area.
pub fn tray() -> Image {
    Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        TRAY_PIXELS,
        TRAY_SIZE,
        TRAY_SIZE,
    ))
}

/// Gives the settings window the program's icon, in its title bar and on the taskbar.
pub fn set_on_window(window: &Window) {
    if let Ok(handle) = window.window_handle()
        && let RawWindowHandle::Win32(handle) = handle.as_raw()
    {
        window_icon::use_program_icon(handle.hwnd.get());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_icon_file_holds_exactly_its_pixels() {
        assert_eq!(TRAY_PIXELS.len() as u32, TRAY_SIZE * TRAY_SIZE * 4);
    }
}
