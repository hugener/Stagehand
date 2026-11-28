//! Keeping an icon in the notification area of the Windows taskbar.
//!
//! The taskbar forgets every icon when it starts again, and it can start after
//! Stagehand does: at sign-in, or when Explorer is restarted. It announces itself to
//! every top-level window when it is ready, which is when icons have to be added again.

use std::io;
use std::sync::OnceLock;

use windows::Win32::Foundation::{ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, FindWindowExW, GWLP_USERDATA,
    GetWindowLongPtrW, GetWindowThreadProcessId, HWND_MESSAGE, RegisterClassExW,
    RegisterWindowMessageW, SetWindowLongPtrW, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP,
};
use windows::core::{PCWSTR, w};

const WINDOW_CLASS: PCWSTR = w!("StagehandTaskbarWatch");

/// The class of the window the UI toolkit keeps for its tray icon. This is the toolkit's
/// own name for it, not something it promises, so it has to be looked at again when the
/// toolkit is updated: see `items/system_tray/windows.rs` in i-slint-core.
const TRAY_ICON_WINDOW_CLASS: PCWSTR = w!("SlintSystemTrayWindow");

type Callback = Box<dyn Fn()>;

/// Calls back every time the taskbar has come into being, until it is dropped.
pub struct TaskbarWatch {
    window: HWND,
    // Read by the window procedure through the window, so it has to stay where it is
    // for as long as the window exists.
    _on_created: Box<Callback>,
}

impl TaskbarWatch {
    /// Starts watching. `on_created` runs on the calling thread, from inside its message
    /// loop, which is no place to do much: hand the work on from there.
    pub fn start(on_created: impl Fn() + 'static) -> io::Result<Self> {
        // SAFETY: asking for the module of this program has no preconditions.
        let module = unsafe { GetModuleHandleW(None) }?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_procedure),
            hInstance: module.into(),
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        // SAFETY: `class` is filled in and its strings are static.
        if unsafe { RegisterClassExW(&class) } == 0 {
            let error = windows::core::Error::from_thread();
            // A second watch in the same process finds the class already there.
            if error.code() != ERROR_CLASS_ALREADY_EXISTS.to_hresult() {
                return Err(error.into());
            }
        }
        // Made before the window, so the taskbar's message is known by the time the
        // window can receive it.
        taskbar_created();

        // A window of its own that is never shown. It must not be a message-only
        // window: those are left out when a message is sent to every window.
        // SAFETY: the class was registered above and every other argument is a plain
        // value.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                WINDOW_CLASS,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module.into()),
                None,
            )
        }?;
        let on_created: Box<Callback> = Box::new(Box::new(on_created));
        // SAFETY: `window` was just created by this thread. The pointer stays valid
        // until the window is destroyed, which `drop` does before the box is freed.
        unsafe { SetWindowLongPtrW(window, GWLP_USERDATA, &raw const *on_created as isize) };
        Ok(Self {
            window,
            _on_created: on_created,
        })
    }
}

impl Drop for TaskbarWatch {
    fn drop(&mut self) {
        // SAFETY: the window belongs to this thread. It is detached from the callback
        // first, so a message already on its way finds nothing to call.
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.window);
        }
    }
}

/// The number of the message the taskbar sends to every window when it is ready, or
/// zero if Windows would not give one.
fn taskbar_created() -> u32 {
    static MESSAGE: OnceLock<u32> = OnceLock::new();
    // SAFETY: the name is a static string.
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) })
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message != 0 && message == taskbar_created() {
        // SAFETY: the value is either zero or the pointer `start` stored, which is
        // valid for as long as the window has it.
        let on_created = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as *const Callback;
        // SAFETY: see above; a null pointer gives `None`.
        if let Some(on_created) = unsafe { on_created.as_ref() } {
            on_created();
        }
        return LRESULT(0);
    }
    // SAFETY: the arguments are passed on exactly as they were received.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Whether this program has the window the UI toolkit keeps for its tray icon. The
/// toolkit only keeps that window when the taskbar took the icon, and reports nothing
/// when it did not, so this is the way to find out.
pub fn tray_icon_window_exists() -> bool {
    // SAFETY: no preconditions.
    let this_process = unsafe { GetCurrentProcessId() };
    let mut after = None;
    loop {
        // SAFETY: the class name is a static string and the handles come from the
        // system.
        let found = unsafe {
            FindWindowExW(
                Some(HWND_MESSAGE),
                after,
                TRAY_ICON_WINDOW_CLASS,
                PCWSTR::null(),
            )
        };
        let Ok(window) = found else {
            return false;
        };
        let mut process = 0u32;
        // SAFETY: `process` is a valid place for the answer.
        unsafe { GetWindowThreadProcessId(window, Some(&raw mut process)) };
        if process == this_process {
            return true;
        }
        after = Some(window);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use windows::Win32::UI::WindowsAndMessaging::SendMessageW;

    use super::*;

    #[test]
    fn the_callback_runs_when_the_taskbar_announces_itself() {
        let calls = Rc::new(Cell::new(0));
        let watch = TaskbarWatch::start({
            let calls = Rc::clone(&calls);
            move || calls.set(calls.get() + 1)
        })
        .unwrap();

        // Sent to this one window only, so no other program hears of it.
        // SAFETY: the window belongs to this thread and the message carries nothing.
        unsafe { SendMessageW(watch.window, taskbar_created(), None, None) };
        assert_eq!(calls.get(), 1);

        // Any other message is not the taskbar's.
        // SAFETY: as above.
        unsafe { SendMessageW(watch.window, taskbar_created() + 1, None, None) };
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_second_watch_can_be_started_after_the_first() {
        drop(TaskbarWatch::start(|| {}).unwrap());

        TaskbarWatch::start(|| {}).unwrap();
    }

    #[test]
    fn a_test_has_no_tray_icon() {
        assert!(!tray_icon_window_exists());
    }
}
