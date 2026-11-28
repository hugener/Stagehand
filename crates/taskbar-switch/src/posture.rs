//! Watching the device for switches between keyboard and tablet posture.

use std::cell::Cell;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::UI::ViewManagement::{UIViewSettings, UserInteractionMode};
use windows::Win32::Devices::Display::{
    AR_LAPTOP, AR_NOSENSOR, AR_NOT_SUPPORTED, AR_STATE, GetAutoRotationState,
};
use windows::Win32::Foundation::{
    ERROR_SUCCESS, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{HKEY, REG_NOTIFY_CHANGE_LAST_SET, RegNotifyChangeKeyValue};
use windows::Win32::System::Threading::{CreateEventW, SetEvent};
use windows::Win32::System::WinRT::{
    IUIViewSettingsInterop, RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetSystemMetrics, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    RegisterClassExW, SM_CONVERTIBLESLATEMODE, TranslateMessage, UnregisterClassW, WM_QUIT,
    WM_SETTINGCHANGE, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use crate::settings::{Posture, Trigger};

const WINDOW_CLASS: PCWSTR = w!("StagehandTaskbarSwitchPostureWatcher");

/// The posture is watched through notifications, but not every device announces a
/// folded keyboard, so the signals are read again on this interval as a safety net.
const RECHECK_INTERVAL_MS: u32 = 5000;

/// How many rechecks the placement made at start is tried again on, until it is seen
/// to have worked. Stagehand may start while Windows is still signing in, when the
/// shell does not yet act on being asked to move the taskbar. A minute covers that
/// without holding on to the taskbar for long.
const START_RETRIES: u32 = 12;

/// The shell keeps its tablet posture flag here; it is the same bit the taskbar and
/// the touch keyboard react to.
const IMMERSIVE_SHELL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\ImmersiveShell";
const TABLET_MODE_VALUE: &str = "TabletMode";

thread_local! {
    // Set by the window procedure and read by the loop that dispatched to it, which
    // are always on the same thread.
    static SETTING_CHANGED: Cell<bool> = const { Cell::new(false) };
}

/// Reports the posture of the device, on a thread of its own, until it is dropped.
pub struct PostureWatcher {
    thread: Option<JoinHandle<()>>,
    stop: Arc<Event>,
    reapply: Arc<Event>,
}

impl PostureWatcher {
    /// Starts watching. `on_posture` is called once with the posture the device is in
    /// right now, and then on every change. It runs on the watcher thread, which has
    /// the Windows Runtime initialised.
    ///
    /// `on_posture` returns whether it is done with what it was told. The call made at
    /// the start is repeated for a while, as `Trigger::Started`, until it is.
    pub fn start(
        on_posture: impl FnMut(Posture, Trigger) -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let stop = Arc::new(Event::new()?);
        let reapply = Arc::new(Event::new()?);
        let (ready_sender, ready) = mpsc::sync_channel(1);

        let thread = thread::Builder::new()
            .name("taskbar-switch".to_owned())
            .spawn({
                let stop = Arc::clone(&stop);
                let reapply = Arc::clone(&reapply);
                move || run(&stop, &reapply, on_posture, &ready_sender)
            })?;

        match ready.recv() {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
                stop,
                reapply,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other(
                    "the posture watcher stopped while starting",
                ))
            }
        }
    }

    /// Asks for the callback to run again with the current posture, as
    /// `Trigger::SettingsChanged`. Used when the settings change, so the taskbar is
    /// always moved from the watcher thread.
    pub fn request_reapply(&self) {
        self.reapply.set();
    }
}

impl Drop for PostureWatcher {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    stop: &Event,
    reapply: &Event,
    mut on_posture: impl FnMut(Posture, Trigger) -> bool,
    ready: &mpsc::SyncSender<io::Result<()>>,
) {
    let _runtime = match Runtime::initialise() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let window = match HiddenWindow::create() {
        Ok(window) => window,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let _ = ready.send(Ok(()));

    // The registry flag is watched as well, because a posture change driven by the
    // shell (rather than by the hardware) does not always broadcast a setting change.
    let shell_key = windows_registry::CURRENT_USER
        .open(IMMERSIVE_SHELL_KEY)
        .ok();
    let registry_changed = shell_key
        .as_ref()
        .and_then(|key| Event::new().ok().map(|event| (key, event)))
        .filter(|(key, event)| watch_registry(key, event));

    let mut handles = vec![stop.raw(), reapply.raw()];
    if let Some((_, event)) = &registry_changed {
        handles.push(event.raw());
    }
    const STOP: u32 = 0;
    const REAPPLY: u32 = 1;
    const REGISTRY: u32 = 2;
    let messages = handles.len() as u32;

    let mut posture = read_and_log(&window, shell_key.as_ref());
    let mut start_retries = retries_after(on_posture(posture, Trigger::Started), START_RETRIES);

    loop {
        // SAFETY: every handle in `handles` is an open event that outlives the loop.
        let result = unsafe {
            MsgWaitForMultipleObjectsEx(
                Some(&handles),
                RECHECK_INTERVAL_MS,
                QS_ALLINPUT,
                MWMO_INPUTAVAILABLE,
            )
        };

        let mut reapply_requested = false;
        let timed_out = result == WAIT_TIMEOUT;
        let check = if timed_out {
            true
        } else {
            match result.0.wrapping_sub(WAIT_OBJECT_0.0) {
                STOP => break,
                REAPPLY => {
                    reapply_requested = true;
                    true
                }
                REGISTRY if registry_changed.is_some() => {
                    // A registry notification fires once and has to be asked for again.
                    if let Some((key, event)) = &registry_changed {
                        watch_registry(key, event);
                    }
                    true
                }
                index if index == messages => {
                    if !pump_messages() {
                        break;
                    }
                    SETTING_CHANGED.replace(false)
                }
                _ => {
                    log::error!(
                        "the posture watcher could not wait for changes: {}",
                        io::Error::last_os_error()
                    );
                    break;
                }
            }
        };
        if !check {
            continue;
        }

        let reading = read_posture(&window, shell_key.as_ref());
        if reading.posture != posture {
            posture = reading.posture;
            reading.log();
            // What happens from here on replaces what was wanted at the start.
            start_retries = 0;
            on_posture(posture, Trigger::PostureChanged);
        } else if reapply_requested {
            start_retries = 0;
            on_posture(posture, Trigger::SettingsChanged);
        } else if timed_out && start_retries > 0 {
            // Only on the timer, so a burst of notifications does not use them all up.
            start_retries = retries_after(on_posture(posture, Trigger::Started), start_retries - 1);
        }
    }
}

/// How many more times the call made at the start is repeated, after one that was
/// `done` or not and with `left` to go otherwise.
fn retries_after(done: bool, left: u32) -> u32 {
    if done { 0 } else { left }
}

/// Dispatches everything waiting in the thread's message queue. Returns false when the
/// thread was asked to quit.
fn pump_messages() -> bool {
    let mut message = MSG::default();
    // SAFETY: `message` is a valid place for the message, and the messages are for
    // windows owned by this thread.
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        if message.message == WM_QUIT {
            return false;
        }
        // SAFETY: `message` was just filled in by PeekMessageW.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    true
}

/// Asks for `event` to be set the next time a value under `key` changes.
fn watch_registry(key: &windows_registry::Key, event: &Event) -> bool {
    // SAFETY: `key` is an open key with notify access and `event` an open event; both
    // outlive the notification, which is cancelled when the key is closed.
    let result = unsafe {
        RegNotifyChangeKeyValue(
            HKEY(key.as_raw()),
            false,
            REG_NOTIFY_CHANGE_LAST_SET,
            Some(event.raw()),
            true,
        )
    };
    result == ERROR_SUCCESS
}

struct Reading {
    posture: Posture,
    touch_interaction: Option<bool>,
    shell_tablet_mode: Option<bool>,
    slate_mode: Option<bool>,
}

impl Reading {
    fn log(&self) {
        let signal = |signal: Option<bool>| match signal {
            Some(true) => "tablet",
            Some(false) => "keyboard",
            None => "unavailable",
        };
        log::info!(
            "device is in {} posture (interaction mode: {}, shell tablet mode: {}, slate mode: {})",
            self.posture.name(),
            signal(self.touch_interaction),
            signal(self.shell_tablet_mode),
            signal(self.slate_mode),
        );
    }
}

fn read_and_log(window: &HiddenWindow, shell_key: Option<&windows_registry::Key>) -> Posture {
    let reading = read_posture(window, shell_key);
    reading.log();
    reading.posture
}

/// Any signal that reads as tablet is enough. Windows only enters its own tablet
/// posture on some devices, while the firmware reports the folded or detached keyboard
/// on every convertible, so the signals are combined rather than ranked.
fn read_posture(window: &HiddenWindow, shell_key: Option<&windows_registry::Key>) -> Reading {
    let touch_interaction = touch_interaction_mode(window);
    let shell_tablet_mode = shell_key
        .and_then(|key| key.get_u32(TABLET_MODE_VALUE).ok())
        .map(|value| value != 0);
    let slate_mode = slate_mode();

    let tablet = [touch_interaction, shell_tablet_mode, slate_mode].contains(&Some(true));
    Reading {
        posture: if tablet {
            Posture::Tablet
        } else {
            Posture::Keyboard
        },
        touch_interaction,
        shell_tablet_mode,
        slate_mode,
    }
}

/// What the shell thinks the user is doing on the display the window is on. Not every
/// convertible makes the shell switch to touch when the keyboard is folded away, so
/// this is one signal among several rather than the answer on its own.
fn touch_interaction_mode(window: &HiddenWindow) -> Option<bool> {
    let interop = windows::core::factory::<UIViewSettings, IUIViewSettingsInterop>().ok()?;
    // SAFETY: the window is alive for as long as `window` is borrowed.
    let settings: UIViewSettings = unsafe { interop.GetForWindow(window.0) }.ok()?;
    Some(settings.UserInteractionMode().ok()? == UserInteractionMode::Touch)
}

/// The firmware's view of whether the keyboard is in use. It only means something on
/// devices that report a rotation state, so desktops are not mistaken for tablets.
fn slate_mode() -> Option<bool> {
    let mut state = AR_STATE::default();
    // SAFETY: `state` is a valid place for the rotation state.
    if !unsafe { GetAutoRotationState(&mut state) }.as_bool() {
        return None;
    }
    if state.0 & (AR_NOT_SUPPORTED.0 | AR_NOSENSOR.0 | AR_LAPTOP.0) != 0 {
        return None;
    }
    // SAFETY: the call takes no pointers and has no preconditions.
    Some(unsafe { GetSystemMetrics(SM_CONVERTIBLESLATEMODE) } == 0)
}

/// Windows Runtime initialisation for the current thread, undone when dropped.
struct Runtime;

impl Runtime {
    fn initialise() -> io::Result<Self> {
        // SAFETY: called on a thread this module started, which has not initialised
        // the Windows Runtime in any other mode.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(io::Error::from)?;
        Ok(Self)
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: balances the successful RoInitialize in `initialise`, on the same
        // thread, after everything that used the runtime on it has been dropped.
        unsafe { RoUninitialize() };
    }
}

/// A window that is never shown. It is a top-level window rather than a message-only
/// one because setting change broadcasts do not reach message-only windows.
struct HiddenWindow(HWND);

impl HiddenWindow {
    fn create() -> io::Result<Self> {
        let instance = module_instance()?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_procedure),
            hInstance: instance,
            lpszClassName: WINDOW_CLASS,
            ..Default::default()
        };
        // SAFETY: `class` is fully initialised and its strings are static.
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the class was registered above and every string is static.
        let window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                WINDOW_CLASS,
                WINDOW_CLASS,
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        };
        match window {
            Ok(window) => Ok(Self(window)),
            Err(error) => {
                // SAFETY: the class was registered above and has no windows.
                let _ = unsafe { UnregisterClassW(WINDOW_CLASS, Some(instance)) };
                Err(error.into())
            }
        }
    }
}

impl Drop for HiddenWindow {
    fn drop(&mut self) {
        // SAFETY: the window was created by this thread and is destroyed once, and the
        // class has no other windows.
        unsafe {
            let _ = DestroyWindow(self.0);
            if let Ok(instance) = module_instance() {
                let _ = UnregisterClassW(WINDOW_CLASS, Some(instance));
            }
        }
    }
}

fn module_instance() -> io::Result<HINSTANCE> {
    // SAFETY: asking for the handle of the running executable has no preconditions.
    let module = unsafe { GetModuleHandleW(None) }?;
    Ok(module.into())
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_SETTINGCHANGE && is_posture_setting(lparam) {
        SETTING_CHANGED.set(true);
    }
    // SAFETY: the arguments are passed on exactly as Windows supplied them.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}

/// Whether a setting change broadcast can mean that the posture changed.
fn is_posture_setting(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return true;
    }
    let setting = PCWSTR(lparam.0 as *const u16);
    // SAFETY: for a setting change, a non-zero parameter points to a nul-terminated
    // wide string that is valid while the message is being handled.
    let setting = unsafe { setting.as_wide() };
    ["UserInteractionMode", "ConvertibleSlateMode"]
        .iter()
        .any(|name| name.encode_utf16().eq(setting.iter().copied()))
}

/// An auto-reset event.
struct Event(OwnedHandle);

impl Event {
    fn new() -> io::Result<Self> {
        // SAFETY: the security attributes and the name are allowed to be absent.
        let handle = unsafe { CreateEventW(None, false, false, None) }?;
        // SAFETY: `handle` was just returned by CreateEventW and is owned by nothing
        // else.
        Ok(Self(unsafe { OwnedHandle::from_raw_handle(handle.0) }))
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }

    fn set(&self) {
        // SAFETY: the handle is an open event for as long as `self` lives.
        let _ = unsafe { SetEvent(self.raw()) };
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn reports_the_posture_on_start_and_again_on_request() {
        let (sender, receiver) = mpsc::channel();
        let watcher = PostureWatcher::start(move |posture, trigger| {
            let _ = sender.send((posture, trigger));
            true
        })
        .unwrap();

        let (posture, trigger) = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(trigger, Trigger::Started);

        watcher.request_reapply();
        let (again, trigger) = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(trigger, Trigger::SettingsChanged);
        assert_eq!(again, posture);

        drop(watcher);
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn the_call_at_the_start_is_repeated_until_it_is_done_or_the_retries_run_out() {
        assert_eq!(retries_after(true, START_RETRIES), 0);
        assert_eq!(retries_after(false, START_RETRIES), START_RETRIES);

        let mut left = retries_after(false, START_RETRIES);
        let mut calls = 0;
        while left > 0 {
            calls += 1;
            left = retries_after(false, left - 1);
        }
        assert_eq!(calls, START_RETRIES);
    }

    #[test]
    fn only_posture_settings_count_as_posture_changes() {
        let lparam = |text: PCWSTR| LPARAM(text.as_ptr() as isize);

        assert!(is_posture_setting(LPARAM(0)));
        assert!(is_posture_setting(lparam(w!("UserInteractionMode"))));
        assert!(is_posture_setting(lparam(w!("ConvertibleSlateMode"))));
        assert!(!is_posture_setting(lparam(w!("ImmersiveColorSet"))));
    }
}
