//! The thread that owns the mirror and its outline, and decides what is shared.
//!
//! Sharing is independent of the tools that draw: a presenter turns it on once, shares
//! the mirror in the call, and leaves it up for the whole session while the laser comes
//! and goes.

use std::cell::{Cell, RefCell};
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use stagehand_core::{Annotations, Shortcut};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CHILDID_SELF, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
    EVENT_SYSTEM_FOREGROUND, GetMessageW, HWND_MESSAGE, KillTimer, MSG, OBJID_WINDOW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetTimer, TranslateMessage, UnregisterClassW,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_DESTROY, WM_HOTKEY, WM_TIMER,
    WNDCLASSEXW,
};
use windows::core::{PCWSTR, w};

use super::border::{self, Border};
use super::mirror::{self, Mirror, WM_MIRROR_CLOSED};
use super::target;
use crate::settings::PresenterWindowSettings;

const WINDOW_CLASS: PCWSTR = w!("StagehandPresenterControl");

/// Share or move to the window under the pointer.
const WM_SHARE: u32 = WM_APP;
/// Share or move to the window the user was working in before opening another one of
/// Stagehand's, such as the tray menu.
const WM_SHARE_PREVIOUS: u32 = WM_APP + 1;
const WM_STOP: u32 = WM_APP + 2;
const WM_APPLY_SETTINGS: u32 = WM_APP + 3;
const WM_TERMINATE: u32 = WM_APP + 4;

const HOTKEY_SHARE: i32 = 1;
const HOTKEY_STOP: i32 = 2;

const RENDER_TIMER_ID: usize = 301;
/// Timers are rounded to the system tick of about 15.6 ms, which is close enough to 60
/// frames per second.
const RENDER_TIMER_INTERVAL_MS: u32 = 16;

const PICK_TIMER_ID: usize = 302;
const PICK_INTERVAL_MS: u32 = 50;
const PICK_TIMEOUT_MS: u64 = 750;

/// How long the name of the mirrored window stays on screen. The outline has no limit:
/// like the one Teams draws, it stays for as long as sharing does.
const NOTICE_MS: u64 = 2500;

thread_local! {
    // The presenter belongs to the thread that runs it. The window procedure is called
    // by Windows with no way to hand it a value, so it finds it here.
    static PRESENTER: RefCell<Option<Presenter>> = const { RefCell::new(None) };

    // The last application window seen in front. Stagehand's own windows and the shell
    // are never recorded, so it keeps pointing at what the user was working in before
    // reaching for the tray.
    static LAST_FOREGROUND: Cell<Option<isize>> = const { Cell::new(None) };
}

/// Runs `action` on this thread's presenter. `None` when there is none, or when it is
/// already in use further up the call stack.
fn with_presenter<R>(action: impl FnOnce(&mut Presenter) -> R) -> Option<R> {
    PRESENTER.with(|presenter| presenter.try_borrow_mut().ok()?.as_mut().map(action))
}

#[derive(Default)]
struct Shared {
    pending_settings: Mutex<Option<PresenterWindowSettings>>,
}

/// The presenter, running on its own thread until this is dropped.
pub struct PresenterThread {
    thread: Option<JoinHandle<()>>,
    // A window handle, kept as a number so this can be owned by another thread.
    window: isize,
    shared: Arc<Shared>,
}

impl PresenterThread {
    pub fn start(settings: PresenterWindowSettings, annotations: Annotations) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let (ready_sender, ready) = mpsc::sync_channel(1);

        let thread = thread::Builder::new()
            .name("presenter-window".to_owned())
            .spawn({
                let shared = Arc::clone(&shared);
                move || run(settings, annotations, shared, &ready_sender)
            })?;

        match ready.recv() {
            Ok(Ok(window)) => Ok(Self {
                thread: Some(thread),
                window,
                shared,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other(
                    "the presenter window stopped while starting",
                ))
            }
        }
    }

    pub fn apply_settings(&self, settings: PresenterWindowSettings) {
        *self
            .shared
            .pending_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(settings);
        self.post(WM_APPLY_SETTINGS);
    }

    pub fn share_previous_window(&self) {
        self.post(WM_SHARE_PREVIOUS);
    }

    pub fn stop_sharing(&self) {
        self.post(WM_STOP);
    }

    fn post(&self, message: u32) {
        // SAFETY: posting to a window of another thread is allowed, and a handle that
        // is no longer a window only makes the call fail.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.window as *mut _)),
                message,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

impl Drop for PresenterThread {
    fn drop(&mut self) {
        // A window has to be destroyed by the thread that created it.
        self.post(WM_TERMINATE);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Windows Runtime initialisation for the current thread, undone when dropped. Window
/// capture is part of the Windows Runtime.
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

fn run(
    settings: PresenterWindowSettings,
    annotations: Annotations,
    shared: Arc<Shared>,
    ready: &mpsc::SyncSender<io::Result<isize>>,
) {
    // The mirrored window's position and size are read in real pixels, so the windows
    // made here must not be scaled by the system.
    // SAFETY: only changes how this thread's own windows see coordinates.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let _runtime = match Runtime::initialise() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let (instance, window, border) = match create_windows() {
        Ok(created) => created,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    // Recorded from the start: no event is sent for a window that is already in front.
    LAST_FOREGROUND.set(target::foreground().map(|window| window.0 as isize));
    // SAFETY: the callback is a plain function that lives as long as the process, and
    // the hook is removed before this thread ends. Out of context means it is called
    // on this thread, from its message loop.
    let foreground_hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_changed),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };

    let presenter = Presenter {
        instance,
        window,
        settings,
        shared,
        annotations,
        mirror: None,
        border,
        notice_until_ms: 0,
        pick_deadline_ms: 0,
        shut_down: false,
    };
    presenter.update_hotkeys();
    PRESENTER.set(Some(presenter));
    let _ = ready.send(Ok(window.0 as isize));

    let mut message = MSG::default();
    // SAFETY: `message` is a valid place for the message, and the messages are for
    // windows and hooks owned by this thread.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        // SAFETY: `message` was just filled in by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    if let Some(mut presenter) = PRESENTER.take() {
        presenter.shut_down();
    }
    // SAFETY: the hook was installed above and is removed once; every window of the
    // classes is gone by now.
    unsafe {
        if !foreground_hook.is_invalid() {
            let _ = UnhookWinEvent(foreground_hook);
        }
        let _ = UnregisterClassW(WINDOW_CLASS, Some(instance));
    }
    mirror::unregister_class(instance);
    border::unregister_class(instance);
}

fn create_windows() -> io::Result<(HINSTANCE, HWND, Border)> {
    // SAFETY: asking for the handle of the running executable has no preconditions.
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
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
    mirror::register_class(instance)?;
    border::register_class(instance)?;

    // Only ever receives messages, timers and shortcuts, so it is never a real window
    // on the desktop.
    // SAFETY: the class was registered above and every string is static.
    let window = unsafe {
        CreateWindowExW(
            Default::default(),
            WINDOW_CLASS,
            WINDOW_CLASS,
            Default::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
    }?;
    let border = Border::create(instance)?;
    Ok((instance, window, border))
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY => {
            with_presenter(|presenter| match wparam.0 as i32 {
                HOTKEY_SHARE => presenter.share(pointed_at_or_last_used()),
                HOTKEY_STOP => presenter.stop_sharing(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_SHARE => {
            with_presenter(|presenter| presenter.share(pointed_at_or_last_used()));
            LRESULT(0)
        }
        WM_SHARE_PREVIOUS => {
            with_presenter(Presenter::share_previous_window);
            LRESULT(0)
        }
        WM_STOP | WM_MIRROR_CLOSED => {
            with_presenter(Presenter::stop_sharing);
            LRESULT(0)
        }
        WM_APPLY_SETTINGS => {
            with_presenter(Presenter::process_pending_settings);
            LRESULT(0)
        }
        WM_TIMER => {
            with_presenter(|presenter| match wparam.0 {
                RENDER_TIMER_ID => presenter.on_render_tick(),
                PICK_TIMER_ID => presenter.on_pick_tick(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_TERMINATE => {
            with_presenter(Presenter::shut_down);
            // Outside the presenter, because destroying the window calls back into
            // this procedure.
            // SAFETY: the window belongs to this thread and is destroyed once.
            let _ = unsafe { DestroyWindow(window) };
            LRESULT(0)
        }
        WM_DESTROY => {
            with_presenter(Presenter::shut_down);
            // SAFETY: ends this thread's own message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: the arguments are passed on exactly as Windows supplied them.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

/// Records what the user is working in. The shell and Stagehand's own windows are not
/// presentable, so opening the tray menu cannot overwrite the answer with itself.
unsafe extern "system" fn foreground_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    window: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if object == OBJID_WINDOW.0 && child == CHILDID_SELF as i32 && target::is_presentable(window) {
        LAST_FOREGROUND.set(Some(window.0 as isize));
    }
}

/// The application window that was last in front, if it can still be shared: it may
/// have been closed or minimised since.
fn last_used() -> Option<HWND> {
    LAST_FOREGROUND
        .get()
        .map(|window| HWND(window as *mut _))
        .filter(|window| target::is_presentable(*window))
}

/// The window the shortcut means. Normally that is the one being pointed at, but the
/// pointer and the focus can both be on something that cannot be shared, most often
/// Stagehand's own settings window right after the shortcut was changed there. The
/// window the user was last working in is then the one meant.
fn pointed_at_or_last_used() -> Option<HWND> {
    target::under_pointer_or_foreground().or_else(last_used)
}

fn hotkey_modifiers(shortcut: &Shortcut) -> HOT_KEY_MODIFIERS {
    // Holding the shortcut down must not act over and over.
    let mut modifiers = MOD_NOREPEAT;
    for (held, modifier) in [
        (shortcut.win, MOD_WIN),
        (shortcut.ctrl, MOD_CONTROL),
        (shortcut.alt, MOD_ALT),
        (shortcut.shift, MOD_SHIFT),
    ] {
        if held {
            modifiers |= modifier;
        }
    }
    modifiers
}

fn now_ms() -> u64 {
    // SAFETY: the call takes no arguments and cannot fail.
    unsafe { GetTickCount64() }
}

struct Presenter {
    instance: HINSTANCE,
    window: HWND,
    settings: PresenterWindowSettings,
    shared: Arc<Shared>,
    annotations: Annotations,
    mirror: Option<Mirror>,
    border: Border,
    notice_until_ms: u64,
    pick_deadline_ms: u64,
    shut_down: bool,
}

impl Presenter {
    /// Sharing is one action whichever state it starts from: with nothing shared it
    /// starts, and with something shared it moves.
    fn share(&mut self, target: Option<HWND>) {
        let Some(target) = target else {
            log::info!("nothing was shared: there is no application window to share");
            return;
        };
        let now = now_ms();

        match &mut self.mirror {
            // Already shared. The outline and its label are shown again below, so that
            // pressing the shortcut always answers with which window is shared.
            Some(mirror) if mirror.target() == target => {}
            Some(mirror) => {
                if let Err(error) = mirror.retarget(target, now) {
                    log::error!("could not move the presenter window to another window: {error}");
                    self.stop_sharing();
                    return;
                }
            }
            None => match Mirror::start(self.instance, target, self.window, now) {
                Ok(mirror) => {
                    self.mirror = Some(mirror);
                    // Whatever was drawn before anyone watched is long gone.
                    self.annotations.clear();
                    self.annotations.set_watched(true);
                    // The mirror keeps refreshing with nothing drawn on it, because
                    // the window it mirrors carries on changing.
                    // SAFETY: the window belongs to this thread; the timer is
                    // delivered to it as a message.
                    unsafe {
                        SetTimer(
                            Some(self.window),
                            RENDER_TIMER_ID,
                            RENDER_TIMER_INTERVAL_MS,
                            None,
                        )
                    };
                }
                Err(error) => {
                    log::error!("could not start the presenter window: {error}");
                    return;
                }
            },
        }

        // The mirror is off screen, so without this there would be no telling which
        // window was picked until it is looked for in the call.
        self.notice_until_ms = now + NOTICE_MS;
        if let Some(mirror) = &self.mirror {
            log::info!("presenter window is showing '{}'", mirror.title());
        }
    }

    /// For when sharing is asked for from another window of Stagehand's own, which is
    /// then in front, so that the pointer is over it and not over anything worth
    /// sharing. That window goes away right after, and what comes back to the front is
    /// what the user was working in. This waits for that on a timer.
    fn share_previous_window(&mut self) {
        self.pick_deadline_ms = now_ms() + PICK_TIMEOUT_MS;
        // SAFETY: the window belongs to this thread; the timer is delivered to it as a
        // message.
        unsafe { SetTimer(Some(self.window), PICK_TIMER_ID, PICK_INTERVAL_MS, None) };
    }

    fn on_pick_tick(&mut self) {
        let mut picked = target::foreground();
        if picked.is_none() {
            if now_ms() < self.pick_deadline_ms {
                return;
            }
            // Waiting is not always enough: dismissing a menu by touch leaves the
            // taskbar in front, not the application behind it. The window that was in
            // front before is then shared from memory.
            picked = last_used();
        }
        // SAFETY: stops the timer started in `share_previous_window`.
        let _ = unsafe { KillTimer(Some(self.window), PICK_TIMER_ID) };
        self.share(picked);
    }

    /// Does nothing when nothing is shared, so the shortcut is harmless at any time.
    fn stop_sharing(&mut self) {
        if self.mirror.take().is_none() {
            return;
        }
        self.annotations.set_watched(false);
        self.annotations.clear();
        self.border.hide();
        // SAFETY: stops the timer started in `share`.
        let _ = unsafe { KillTimer(Some(self.window), RENDER_TIMER_ID) };
        log::info!("presenter window stopped");
    }

    fn on_render_tick(&mut self) {
        let Some(mirror) = &mut self.mirror else {
            return;
        };
        let now = now_ms();
        let alive = self.annotations.read(|shapes| mirror.present(shapes, now));
        if !alive {
            log::info!("the shared window is gone");
            self.stop_sharing();
            return;
        }

        let label = (now < self.notice_until_ms).then(|| mirror.title());
        self.border
            .update(mirror.target(), mirror.target_rect(), label, now);
    }

    fn process_pending_settings(&mut self) {
        let pending = self
            .shared
            .pending_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(settings) = pending
            && settings != self.settings
        {
            self.settings = settings;
            self.update_hotkeys();
        }
    }

    fn unregister_hotkeys(&self) {
        for id in [HOTKEY_SHARE, HOTKEY_STOP] {
            // SAFETY: the window belongs to this thread; an id that was never
            // registered only makes the call fail.
            let _ = unsafe { UnregisterHotKey(Some(self.window), id) };
        }
    }

    fn update_hotkeys(&self) {
        self.unregister_hotkeys();
        for (id, shortcut, purpose) in [
            (HOTKEY_SHARE, self.settings.share_shortcut, "share"),
            (HOTKEY_STOP, self.settings.stop_shortcut, "stop sharing"),
        ] {
            let Some(shortcut) = shortcut else { continue };
            if !shortcut.is_usable() {
                log::warn!(
                    "the {purpose} shortcut {shortcut} is ordinary typing and was not registered"
                );
                continue;
            }
            // SAFETY: the window belongs to this thread and outlives the registration,
            // which `shut_down` removes.
            let registered = unsafe {
                RegisterHotKey(
                    Some(self.window),
                    id,
                    hotkey_modifiers(&shortcut),
                    shortcut.key.virtual_key(),
                )
            };
            match registered {
                Ok(()) => log::info!("presenter window {purpose} shortcut is {shortcut}"),
                Err(error) => log::error!(
                    "could not register {shortcut} as the {purpose} shortcut, which usually \
                     means something else is using it: {error}"
                ),
            }
        }
    }

    fn shut_down(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        self.stop_sharing();
        self.unregister_hotkeys();
        // SAFETY: stopping a timer that is not running only makes the call fail.
        let _ = unsafe { KillTimer(Some(self.window), PICK_TIMER_ID) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shortcut_becomes_its_modifiers_without_repeat() {
        let shortcut = Shortcut::parse("Win+Ctrl+Shift+W").unwrap();

        assert_eq!(
            hotkey_modifiers(&shortcut),
            MOD_WIN | MOD_CONTROL | MOD_SHIFT | MOD_NOREPEAT
        );
    }

    #[test]
    fn the_presenter_starts_and_stops() {
        // Tests must not take shortcuts away from a running copy of the app.
        let settings = PresenterWindowSettings {
            share_shortcut: None,
            stop_shortcut: None,
        };
        let annotations = Annotations::new();
        let presenter = PresenterThread::start(settings, annotations.clone()).unwrap();

        presenter.apply_settings(settings);
        presenter.stop_sharing();
        drop(presenter);

        assert!(!annotations.is_watched());
    }
}
