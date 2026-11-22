//! Giving a window the icon that is built into the program.

use std::ffi::c_void;

use windows::Win32::Foundation::HWND;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GCLP_HICON, GCLP_HICONSM, IMAGE_ICON, LR_SHARED, LoadImageW, SetClassLongPtrW,
};
use windows::core::PCWSTR;

/// The number the program's icon is built in under; see the application's build script.
const PROGRAM_ICON: usize = 1;

/// Makes the program's icon the one `window` shows in its title bar and on the taskbar.
/// `window` is the native handle of a window of this program. Without an icon built in,
/// as in a test, the window keeps the system's default.
///
/// The icon is set for the window's class, which is what a window falls back to when it
/// has none of its own. A window toolkit that manages the window's own icon, and clears
/// it when it has none to show, leaves that alone.
pub fn use_program_icon(window: isize) {
    let window = HWND(window as *mut c_void);
    // SAFETY: every call takes plain values. A resource is named by its number in place
    // of a string, which is what the small integer stands for; nothing is read through
    // it. A shared icon is owned by the system and never has to be destroyed.
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        // Zero when the handle is not a window, in which case nothing below applies.
        let dpi = GetDpiForWindow(window) as i32;
        for (which, size) in [(GCLP_HICON, 32), (GCLP_HICONSM, 16)] {
            let size = size * dpi / 96;
            let icon = LoadImageW(
                Some(module.into()),
                PCWSTR(PROGRAM_ICON as *const u16),
                IMAGE_ICON,
                size,
                size,
                LR_SHARED,
            );
            if let Ok(icon) = icon {
                SetClassLongPtrW(window, which, icon.0 as isize);
            }
        }
    }
}
