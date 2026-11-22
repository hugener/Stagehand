//! The see-through window the trail is drawn on, the mouse hook and pen that feed it,
//! and the loop that draws it. The trail itself is modelled in `crate::stroke`.
//!
//! Everything here lives on one thread of its own. A low level mouse hook is called on
//! the thread that installed it, and every moment that thread is busy delays the mouse
//! for the whole system, so it must not share a thread with a user interface.

use std::cell::{Cell, RefCell};
use std::io;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::HRAWINPUT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE,
    GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, HC_ACTION, HHOOK, HTCLIENT,
    HTTRANSPARENT, HWND_TOPMOST, KillTimer, MSG, MSLLHOOKSTRUCT, PostMessageW, PostQuitMessage,
    RegisterClassExW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOREDRAW, SWP_NOSIZE, SetTimer,
    SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, ShowWindow, TranslateMessage,
    UnhookWindowsHookEx, UnregisterClassW, WH_MOUSE_LL, WM_APP, WM_DESTROY, WM_DISPLAYCHANGE,
    WM_DPICHANGED, WM_HOTKEY, WM_INPUT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
    WM_NCHITTEST, WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERENTER, WM_POINTERLEAVE,
    WM_POINTERUP, WM_POINTERUPDATE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WM_XBUTTONDOWN,
    WM_XBUTTONUP, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

use super::cursor::DrawingCursor;
use super::graphics::{Bounds, Graphics, Rgba};
use super::pen::PenInput;
use crate::settings::{LaserPointerSettings, MouseButton};
use crate::stroke::{LaserStroke, StrokeOptions, Vec2};
use stagehand_core::{Annotations, Shape, Shortcut};

const WINDOW_CLASS: PCWSTR = w!("StagehandLaserPointerOverlay");

const WM_SWITCH_MOUSE: u32 = WM_APP;
const WM_APPLY_SETTINGS: u32 = WM_APP + 1;
const WM_WAKE: u32 = WM_APP + 2;
const WM_TERMINATE: u32 = WM_APP + 3;

// What a wake message asks for.
const WAKE_END: usize = 0;
const WAKE_BEGIN: usize = 1;
const WAKE_RESUME: usize = 2;

const HOTKEY_MOUSE: i32 = 1;
const HOTKEY_PEN: i32 = 2;

const RENDER_TIMER_ID: usize = 201;
/// Timers are rounded to the system tick of about 15.6 ms, which lands close enough to
/// 60 frames per second. The timer paces drawing, not the display: see `Graphics::end`.
const RENDER_TIMER_INTERVAL_MS: u32 = 16;

/// Windows tags mouse messages that were made from a pen or from touch with this
/// signature in their extra information. One bit tells touch from pen.
const PEN_OR_TOUCH_SIGNATURE: usize = 0xFF51_5700;
const PEN_OR_TOUCH_SIGNATURE_MASK: usize = 0xFFFF_FF00;
const TOUCH_FLAG: usize = 0x80;

/// Arming and disarming are otherwise invisible, so each plays a short ring at the
/// pointer: it closes in to a dot when arming and grows and dissolves when disarming.
/// It doubles as a "where is my pointer" cue, which is why it starts wide.
const HONE_DURATION_MS: u64 = 400;
// At 100% scaling; multiplied by the scaling of the monitor the pointer is on.
const HONE_RADIUS: f32 = 80.0;
const HONE_DOT_RADIUS: f32 = 3.0;
// The ring is heavier than the trail it announces. It has to read at a glance, and it
// is on screen for well under half a second.
const HONE_THICKNESS_SCALE: f32 = 1.75;
const HONE_MIN_THICKNESS: f32 = 4.0;

// The trail is one solid shape with an optional soft halo behind it.
const GLOW_WIDTH_SCALE: f32 = 2.4;
const GLOW_ALPHA_SCALE: f32 = 0.22;

/// Most positions the hook keeps between two frames. A 1000 Hz mouse produces about 16
/// per frame, so this only matters if drawing stalls badly, and dropping the oldest is
/// the right answer then anyway.
const MAX_PENDING_POINTS: usize = 512;
/// The trail length setting was tuned with one position per frame.
const REFERENCE_SAMPLES_PER_TICK: f32 = 1.0;
/// Trails still fading while a new one is drawn. They expire by themselves, so this
/// only bounds a burst of very fast presses.
const MAX_PAST_STROKES: usize = 8;

/// How long without a word from a hovering pen before its trail is ended.
const PEN_IDLE_TIMEOUT_MS: u64 = 200;
/// How long after its last report the pen still counts as being at the screen. Reports
/// stop the moment it leaves, so this only bridges the gap between two of them.
const PEN_RANGE_TIMEOUT_MS: u64 = 350;
/// How far from the pen's last position the overlay takes input for itself. It only has
/// to cover how far the pen can move between two reports; the rest is margin.
const PEN_CAPTURE_RADIUS: i32 = 64;

thread_local! {
    // The overlay belongs to the thread that runs it. The window procedure and the hook
    // are called by Windows with no way to hand them a value, so they find it here.
    static OVERLAY: RefCell<Option<Overlay>> = const { RefCell::new(None) };

    // Where the overlay currently takes pen input for itself, if anywhere. Kept apart
    // from the overlay so a hit test can be answered while the overlay is busy.
    static PEN_PATCH: Cell<Option<POINT>> = const { Cell::new(None) };
}

/// Runs `action` on this thread's overlay. `None` when there is none, or when it is
/// already in use further up the call stack: Windows calls back into the window
/// procedure from inside calls such as `ShowWindow`.
fn with_overlay<R>(action: impl FnOnce(&mut Overlay) -> R) -> Option<R> {
    OVERLAY.with(|overlay| overlay.try_borrow_mut().ok()?.as_mut().map(action))
}

/// What the plugin and the overlay thread share.
#[derive(Default)]
struct Shared {
    pending_settings: Mutex<Option<LaserPointerSettings>>,
}

/// The overlay, running on its own thread until this is dropped.
pub struct OverlayThread {
    thread: Option<JoinHandle<()>>,
    // A window handle, kept as a number so this can be owned by another thread.
    window: isize,
    shared: Arc<Shared>,
}

impl OverlayThread {
    pub fn start(settings: LaserPointerSettings, annotations: Annotations) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let (ready_sender, ready) = mpsc::sync_channel(1);

        let thread = thread::Builder::new()
            .name("laser-pointer".to_owned())
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
                Err(io::Error::other("the laser pointer stopped while starting"))
            }
        }
    }

    pub fn apply_settings(&self, settings: LaserPointerSettings) {
        *self
            .shared
            .pending_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(settings);
        self.post(WM_APPLY_SETTINGS);
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

impl Drop for OverlayThread {
    fn drop(&mut self) {
        // A window has to be destroyed by the thread that created it.
        self.post(WM_TERMINATE);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    settings: LaserPointerSettings,
    annotations: Annotations,
    shared: Arc<Shared>,
    ready: &mpsc::SyncSender<io::Result<isize>>,
) {
    // SAFETY: only changes how this thread's own windows see coordinates.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };

    let (instance, window) = match create_window() {
        Ok(created) => created,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };

    let auto_activate = settings.auto_activate;
    let mut overlay = Overlay::new(instance, window, settings, annotations, shared);
    overlay.update_hotkeys();
    // A button that draws without a shortcut needs the hook from the start; nothing
    // else will arm to install it.
    overlay.update_hook();
    OVERLAY.set(Some(overlay));
    let _ = ready.send(Ok(window.0 as isize));

    if auto_activate {
        // SAFETY: the window was created above by this thread.
        let _ = unsafe { PostMessageW(Some(window), WM_SWITCH_MOUSE, WPARAM(0), LPARAM(0)) };
    }

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

    if let Some(mut overlay) = OVERLAY.take() {
        overlay.shut_down();
    }
    // SAFETY: the window is gone by the time the loop ends, so the class has none.
    let _ = unsafe { UnregisterClassW(WINDOW_CLASS, Some(instance)) };
}

fn create_window() -> io::Result<(HINSTANCE, HWND)> {
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

    // Transparent lets input through to what is underneath. Without a redirection
    // bitmap the window has no surface of its own, only what is composed onto it. A
    // tool window stays out of the taskbar and the window switcher.
    let style = WS_EX_TRANSPARENT
        | WS_EX_NOREDIRECTIONBITMAP
        | WS_EX_TOOLWINDOW
        | WS_EX_TOPMOST
        | WS_EX_NOACTIVATE;
    // SAFETY: the class was registered above and every string is static.
    let window = unsafe {
        CreateWindowExW(
            style,
            WINDOW_CLASS,
            w!("Stagehand Laser Pointer"),
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
        Ok(window) => Ok((instance, window)),
        Err(error) => {
            // SAFETY: the class was registered above and has no windows.
            let _ = unsafe { UnregisterClassW(WINDOW_CLASS, Some(instance)) };
            Err(error.into())
        }
    }
}

unsafe extern "system" fn window_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => {
            // Everything passes through, except right around an armed pen that is at
            // the screen, which is where its next touch will land.
            let x = i32::from(lparam.0 as u16 as i16);
            let y = i32::from((lparam.0 >> 16) as u16 as i16);
            let near_pen = PEN_PATCH.get().is_some_and(|pen| {
                (x - pen.x).abs() <= PEN_CAPTURE_RADIUS && (y - pen.y).abs() <= PEN_CAPTURE_RADIUS
            });
            if near_pen {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        // Whatever the pen does while the overlay has it ends here. Not passing these
        // on also keeps them from being turned into mouse messages, so the hook does
        // not see the same touch a second time.
        WM_POINTERDOWN
        | WM_POINTERUPDATE
        | WM_POINTERUP
        | WM_POINTERENTER
        | WM_POINTERLEAVE
        | WM_POINTERCAPTURECHANGED => LRESULT(0),
        WM_INPUT => {
            with_overlay(|overlay| overlay.handle_raw_input(HRAWINPUT(lparam.0 as *mut _)));
            // SAFETY: the arguments are passed on exactly as Windows supplied them.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
        WM_HOTKEY => {
            with_overlay(|overlay| match wparam.0 as i32 {
                HOTKEY_MOUSE => overlay.switch_mouse(),
                HOTKEY_PEN => overlay.switch_pen(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_SWITCH_MOUSE => {
            with_overlay(Overlay::switch_mouse);
            LRESULT(0)
        }
        WM_WAKE => {
            with_overlay(|overlay| match wparam.0 {
                WAKE_BEGIN => overlay.begin_drawing(),
                WAKE_END => overlay.end_drawing(),
                WAKE_RESUME => overlay.resume(),
                _ => {}
            });
            LRESULT(0)
        }
        WM_APPLY_SETTINGS => {
            with_overlay(Overlay::process_pending_settings);
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == RENDER_TIMER_ID => {
            with_overlay(Overlay::on_render_tick);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED => {
            with_overlay(Overlay::ensure_geometry);
            LRESULT(0)
        }
        WM_TERMINATE => {
            with_overlay(Overlay::shut_down);
            // Outside the overlay, because destroying the window calls back into this
            // procedure.
            // SAFETY: the window belongs to this thread and is destroyed once.
            let _ = unsafe { DestroyWindow(window) };
            LRESULT(0)
        }
        WM_DESTROY => {
            with_overlay(Overlay::shut_down);
            // SAFETY: ends this thread's own message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: the arguments are passed on exactly as Windows supplied them.
        _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
    }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for this code the parameter points to the event, which stays valid
        // for the duration of the call.
        let event = unsafe { *(lparam.0 as *const MSLLHOOKSTRUCT) };
        let swallow = with_overlay(|overlay| overlay.handle_mouse_input(wparam.0 as u32, &event));
        if swallow == Some(true) {
            return LRESULT(1);
        }
    }
    // SAFETY: the arguments are passed on exactly as Windows supplied them.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn is_from_pen(extra_information: usize) -> bool {
    extra_information & PEN_OR_TOUCH_SIGNATURE_MASK == PEN_OR_TOUCH_SIGNATURE
        && extra_information & TOUCH_FLAG == 0
}

/// The button a low level mouse message is about and whether it went down. `None` for
/// anything that is not a button changing, such as movement and the wheel.
fn classify_button(message: u32, mouse_data: u32) -> Option<(MouseButton, bool)> {
    const XBUTTON1: u32 = 1;
    let side = || {
        if mouse_data >> 16 == XBUTTON1 {
            MouseButton::X1
        } else {
            MouseButton::X2
        }
    };
    Some(match message {
        WM_LBUTTONDOWN => (MouseButton::Left, true),
        WM_LBUTTONUP => (MouseButton::Left, false),
        WM_RBUTTONDOWN => (MouseButton::Right, true),
        WM_RBUTTONUP => (MouseButton::Right, false),
        WM_MBUTTONDOWN => (MouseButton::Middle, true),
        WM_MBUTTONUP => (MouseButton::Middle, false),
        WM_XBUTTONDOWN => (side(), true),
        WM_XBUTTONUP => (side(), false),
        _ => return None,
    })
}

fn hotkey_modifiers(shortcut: &Shortcut) -> HOT_KEY_MODIFIERS {
    // Holding the shortcut down must not arm and disarm over and over.
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

fn cursor_position() -> Option<POINT> {
    let mut position = POINT::default();
    // SAFETY: `position` is a valid place for the position.
    unsafe { GetCursorPos(&mut position) }
        .is_ok()
        .then_some(position)
}

/// How much larger than 100% the monitor under a point shows things.
fn scale_at(point: POINT) -> f32 {
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    // SAFETY: the monitor handle comes straight from the system, and both numbers are
    // live locals.
    let result = unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y)
    };
    if result.is_ok() {
        dpi_x as f32 / 96.0
    } else {
        1.0
    }
}

fn vec2(point: POINT) -> Vec2 {
    Vec2 {
        x: point.x as f32,
        y: point.y as f32,
    }
}

#[derive(Clone, Copy)]
struct Hone {
    start_ms: u64,
    centre: POINT,
    /// False for the ring that closes in when arming, true for the one that grows and
    /// dissolves when disarming.
    expanding: bool,
}

struct Overlay {
    instance: HINSTANCE,
    window: HWND,
    hook: Option<HHOOK>,
    graphics: Option<Graphics>,
    shared: Arc<Shared>,
    settings: LaserPointerSettings,

    stroke: LaserStroke,
    // Trails that were finished while still visible. Starting a new one must not erase
    // one that is still fading, so it is parked here until it is gone.
    past_strokes: Vec<LaserStroke>,
    outline: Vec<Vec2>,
    annotations: Annotations,
    // The shapes handed to `annotations` last, kept to reuse their storage.
    published: Vec<Shape>,
    has_published: bool,

    latest_position: Option<POINT>,
    // Every position seen since the last frame. A mouse reports 125 to 1000 times a
    // second, far more often than a frame is drawn, and keeping only the latest would
    // throw away most of the trail's detail.
    pending_points: Vec<(POINT, u64)>,
    // Smoothed positions per frame, used to keep the trail the same visible length
    // however fast the mouse reports. See `on_render_tick`.
    samples_per_tick: f32,

    // The two shortcuts arm separately, so the pen can be used without the mouse.
    mouse_armed: bool,
    pen_armed: bool,
    activation_button_held: bool,
    always_on_button_held: bool,
    drawing: bool,
    render_loop_running: bool,
    visible: bool,
    hone: Option<Hone>,
    cursor: DrawingCursor,

    pen: PenInput,
    pen_contact: bool,
    last_pen_input_ms: u64,
    last_pen_point: POINT,
    pen_awaiting_first_sample: bool,
    pen_stroke_open: bool,
    pen_capturing: bool,
    shut_down: bool,
}

impl Overlay {
    fn new(
        instance: HINSTANCE,
        window: HWND,
        settings: LaserPointerSettings,
        annotations: Annotations,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            instance,
            window,
            hook: None,
            graphics: None,
            shared,
            settings,
            stroke: LaserStroke::default(),
            past_strokes: Vec::new(),
            outline: Vec::new(),
            annotations,
            published: Vec::new(),
            has_published: false,
            latest_position: None,
            pending_points: Vec::new(),
            samples_per_tick: REFERENCE_SAMPLES_PER_TICK,
            mouse_armed: false,
            pen_armed: false,
            activation_button_held: false,
            always_on_button_held: false,
            drawing: false,
            render_loop_running: false,
            visible: false,
            hone: None,
            cursor: DrawingCursor::new(),
            pen: PenInput::default(),
            pen_contact: false,
            last_pen_input_ms: 0,
            last_pen_point: POINT::default(),
            pen_awaiting_first_sample: false,
            pen_stroke_open: false,
            pen_capturing: false,
            shut_down: false,
        }
    }

    fn post(&self, message: u32, wparam: usize) {
        // SAFETY: the window belongs to this thread and is alive for as long as `self`.
        let _ = unsafe { PostMessageW(Some(self.window), message, WPARAM(wparam), LPARAM(0)) };
    }

    // ---- Surface ----------------------------------------------------------------

    /// Sizes the window and its surface to the whole desktop. Called before every use
    /// and whenever the displays change.
    fn ensure_geometry(&mut self) -> bool {
        // Inset by a pixel: a see-through window covering the whole desktop exactly
        // makes Windows glitch the transparency of the taskbar.
        // SAFETY: the calls take no pointers and have no preconditions.
        let bounds = unsafe {
            Bounds {
                left: GetSystemMetrics(SM_XVIRTUALSCREEN) + 1,
                top: GetSystemMetrics(SM_YVIRTUALSCREEN) + 1,
                width: (GetSystemMetrics(SM_CXVIRTUALSCREEN) - 2).max(1) as u32,
                height: (GetSystemMetrics(SM_CYVIRTUALSCREEN) - 2).max(1) as u32,
            }
        };
        // The origin matters as well as the size: rearranging monitors can move the
        // desktop's top left corner without changing its extent.
        if self.graphics.as_ref().and_then(Graphics::bounds) == Some(bounds) {
            return true;
        }

        // SAFETY: the window belongs to this thread.
        let _ = unsafe {
            SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                bounds.left,
                bounds.top,
                bounds.width as i32,
                bounds.height as i32,
                SWP_NOACTIVATE | SWP_NOREDRAW,
            )
        };

        let window = self.window;
        let result = match self.graphics.take() {
            Some(graphics) => Ok(graphics),
            None => Graphics::new(),
        }
        .and_then(|mut graphics| graphics.resize(window, bounds).map(|()| graphics));
        match result {
            Ok(graphics) => {
                self.graphics = Some(graphics);
                true
            }
            Err(error) => {
                log::error!("could not set up the laser pointer's overlay: {error}");
                false
            }
        }
    }

    fn show(&mut self) {
        if self.visible || !self.ensure_geometry() {
            return;
        }
        // SAFETY: the window belongs to this thread.
        unsafe {
            let _ = ShowWindow(self.window, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(
                self.window,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        self.visible = true;
    }

    fn hide(&mut self) {
        if !self.visible {
            return;
        }
        // The surface keeps what was drawn last, and hiding the window does not discard
        // it: the next time it is shown, the old frame would flash up before a new one
        // replaces it. Blanking on the way out leaves nothing to flash.
        if let Some(graphics) = &self.graphics {
            graphics.begin();
            if !graphics.end() {
                self.graphics = None;
            }
        }
        // SAFETY: the window belongs to this thread.
        let _ = unsafe { ShowWindow(self.window, SW_HIDE) };
        self.visible = false;
    }

    fn start_render_loop(&mut self) {
        if self.render_loop_running {
            return;
        }
        // SAFETY: the window belongs to this thread; the timer is delivered to it as a
        // message.
        let timer = unsafe {
            SetTimer(
                Some(self.window),
                RENDER_TIMER_ID,
                RENDER_TIMER_INTERVAL_MS,
                None,
            )
        };
        self.render_loop_running = timer != 0;
    }

    fn stop_render_loop(&mut self) {
        if !self.render_loop_running {
            return;
        }
        // SAFETY: stops the timer started in `start_render_loop` on the same window.
        let _ = unsafe { KillTimer(Some(self.window), RENDER_TIMER_ID) };
        self.render_loop_running = false;
    }

    fn color(&self) -> Rgba {
        let color = self.settings.color;
        Rgba {
            red: f32::from(color.red) / 255.0,
            green: f32::from(color.green) / 255.0,
            blue: f32::from(color.blue) / 255.0,
            alpha: f32::from(color.alpha) / 255.0,
        }
    }

    fn render(&mut self, now: u64) {
        let Some(graphics) = &self.graphics else {
            return;
        };
        let color = self.color();
        let glow = self.settings.glow_enabled;

        // Someone may be showing a single window to remote viewers, who cannot see this
        // overlay; the trails are then published so they can be repeated for them.
        let publish = self.annotations.is_watched();
        self.published.clear();

        graphics.begin();

        // Each trail is a filled outline built from its centre line, not overlapping
        // stamps, so a see-through colour keeps one even opacity instead of darkening
        // wherever the trail overlaps itself. The soft halo goes underneath the body.
        let halo = (GLOW_WIDTH_SCALE, color.with_alpha_scaled(GLOW_ALPHA_SCALE));
        let body = (1.0, color);
        let passes = if glow { &[halo, body][..] } else { &[body][..] };
        for stroke in self
            .past_strokes
            .iter_mut()
            .chain(std::iter::once(&mut self.stroke))
        {
            for &(width_scale, color) in passes {
                if stroke.build_outline(now, &mut self.outline, width_scale) < 3 {
                    continue;
                }
                graphics.fill_polygon(&self.outline, color);
                if publish {
                    self.published.push(Shape {
                        outline: self
                            .outline
                            .iter()
                            .map(|point| (point.x, point.y))
                            .collect(),
                        color: [color.red, color.green, color.blue, color.alpha],
                    });
                }
            }
        }
        if publish || self.has_published {
            self.annotations.publish(&mut self.published);
            self.has_published = publish;
        }

        if let Some(hone) = self.hone.filter(|hone| hone_active(hone, now)) {
            let progress = (now - hone.start_ms) as f32 / HONE_DURATION_MS as f32;
            // Ease out, so the ring moves fastest at the start and settles at the end.
            let eased = 1.0 - (1.0 - progress) * (1.0 - progress);

            let scale = scale_at(hone.centre);
            let wide = HONE_RADIUS * scale;
            let dot = HONE_DOT_RADIUS * scale;
            let (radius, fade) = if hone.expanding {
                // Disarming: out from the dot, dissolving the whole way.
                (dot + (wide - dot) * eased, 1.0 - eased)
            } else {
                // Arming: in to the dot, keeping its colour until it is nearly there so
                // the eye can follow it, then gone.
                let fade = if progress < 0.75 {
                    1.0
                } else {
                    (1.0 - progress) / 0.25
                };
                (wide - (wide - dot) * eased, fade)
            };

            let thickness =
                (self.settings.size as f32 * HONE_THICKNESS_SCALE).max(HONE_MIN_THICKNESS) * scale;
            let color = color.with_alpha_scaled(fade);
            let centre = vec2(hone.centre);
            if glow {
                graphics.draw_ring(
                    centre,
                    radius,
                    thickness * GLOW_WIDTH_SCALE,
                    color.with_alpha_scaled(GLOW_ALPHA_SCALE),
                );
            }
            graphics.draw_ring(centre, radius, thickness, color);
        }

        if !graphics.end() {
            log::warn!("the laser pointer lost its graphics device and will set it up again");
            self.graphics = None;
        }
    }

    // ---- Arming -----------------------------------------------------------------

    fn start_hone(&mut self, expanding: bool) {
        let Some(centre) = cursor_position() else {
            return;
        };
        // Arming draws nothing by itself, so the overlay is normally down at this point
        // and has to come up for the animation alone.
        if !self.ensure_geometry() {
            return;
        }
        self.hone = Some(Hone {
            start_ms: now_ms(),
            centre,
            expanding,
        });
        self.show();
        self.start_render_loop();
    }

    fn hone_running(&self, now: u64) -> bool {
        self.hone.is_some_and(|hone| hone_active(&hone, now))
    }

    /// The pen shape says the mouse is a laser, which is true from the moment the
    /// shortcut arms it and not only while a trail is drawn. The always-on button has
    /// no armed state, so for that one the trail is all there is to go on.
    fn update_drawing_cursor(&mut self) {
        self.cursor
            .set_shown(self.mouse_armed || self.always_on_button_held);
    }

    /// The hook is what costs something, so it is only installed while something can
    /// start a trail: an armed mouse, an armed pen, or an always-on button.
    fn hook_needed(&self) -> bool {
        self.mouse_armed || self.pen_armed || self.settings.always_on_button.is_some()
    }

    fn update_hook(&mut self) {
        let needed = self.hook_needed();
        if needed && self.hook.is_none() {
            if !self.ensure_geometry() {
                return;
            }
            // SAFETY: the hook procedure is a plain function that lives as long as the
            // process, and the hook is removed before this thread ends.
            match unsafe {
                SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), Some(self.instance), 0)
            } {
                Ok(hook) => self.hook = Some(hook),
                Err(error) => log::error!("could not listen to the mouse: {error}"),
            }
        } else if !needed {
            self.remove_hook();
        }
    }

    fn remove_hook(&mut self) {
        if let Some(hook) = self.hook.take() {
            // SAFETY: the hook was installed by `update_hook` and is removed once.
            let _ = unsafe { UnhookWindowsHookEx(hook) };
        }
    }

    fn switch_mouse(&mut self) {
        if self.mouse_armed {
            self.disarm_mouse();
        } else {
            self.arm_mouse();
        }
    }

    fn switch_pen(&mut self) {
        if self.pen_armed {
            self.disarm_pen();
        } else {
            self.arm_pen();
        }
    }

    /// The mouse shortcut arms only the mouse. Arming only starts listening: nothing is
    /// drawn until the activation button is held down.
    fn arm_mouse(&mut self) {
        self.mouse_armed = true;
        self.activation_button_held = false;
        self.update_hook();
        if self.hook.is_none() {
            self.mouse_armed = false;
            return;
        }
        // Without these there would be no sign that the shortcut did anything.
        self.start_hone(false);
        self.update_drawing_cursor();
        log::info!("laser pointer armed for the mouse");
    }

    fn disarm_mouse(&mut self) {
        if !self.mouse_armed {
            return;
        }
        self.mouse_armed = false;
        self.activation_button_held = false;
        // Done here and not left to `release_if_idle`, which stops early when the pen
        // is still armed and would leave the pen cursor behind.
        self.update_drawing_cursor();
        self.start_hone(true);
        self.update_hook();
        self.release_if_idle();
        log::info!("laser pointer disarmed for the mouse");
    }

    fn arm_pen(&mut self) {
        self.pen_armed = true;
        self.pen_contact = false;
        self.last_pen_input_ms = 0;
        self.pen_awaiting_first_sample = false;
        self.pen_stroke_open = false;
        self.update_hook();
        self.pen.register(self.window);
        log::info!("laser pointer armed for the pen");
    }

    fn disarm_pen(&mut self) {
        if !self.pen_armed {
            return;
        }
        self.pen_armed = false;
        self.pen_contact = false;
        self.update_pen_capture(now_ms());
        self.pen.unregister();
        self.update_hook();
        self.release_if_idle();
        if !self.mouse_armed && !self.drawing && !self.hone_running(now_ms()) {
            self.hide();
        }
        log::info!("laser pointer disarmed for the pen");
    }

    /// Drops what is still on screen once nothing can start a trail. A held always-on
    /// button is not something a shortcut arms, so it keeps the overlay alive by itself.
    fn release_if_idle(&mut self) {
        if self.mouse_armed || self.pen_armed || self.always_on_button_held {
            return;
        }
        self.drawing = false;
        self.cursor.restore();
        self.pending_points.clear();
        self.stroke.clear();
        self.past_strokes.clear();

        // The ring shown when disarming is started right before this runs, and needs
        // the surface to stay up.
        if self.hone_running(now_ms()) {
            return;
        }
        self.stop_render_loop();
        self.hide();
    }

    /// Lets go of everything, for when the tool is switched off. Unlike disarming, this
    /// removes the hook whatever the settings say.
    fn shut_down(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        self.mouse_armed = false;
        self.pen_armed = false;
        self.update_pen_capture(now_ms());
        self.drawing = false;
        self.activation_button_held = false;
        self.always_on_button_held = false;
        self.pen_contact = false;
        self.pending_points.clear();
        self.stroke.clear();
        self.past_strokes.clear();
        self.hone = None;
        if self.has_published {
            self.annotations.clear();
            self.has_published = false;
        }

        self.cursor.restore();
        self.pen.unregister();
        self.remove_hook();
        self.unregister_hotkeys();
        self.stop_render_loop();
        self.hide();
        self.graphics = None;
    }

    // ---- Drawing ----------------------------------------------------------------

    fn begin_drawing(&mut self) {
        if self.drawing {
            return;
        }
        self.drawing = true;

        // Park a trail that is still visible instead of wiping it, so pressing the
        // button again right away does not chop off what is already on screen.
        if !self.stroke.is_empty() {
            self.stroke.finish();
            let options = *self.stroke.options();
            let finished = std::mem::replace(&mut self.stroke, LaserStroke::new(options));
            self.past_strokes.push(finished);
            if self.past_strokes.len() > MAX_PAST_STROKES {
                self.past_strokes.remove(0);
            }
        }
        self.stroke.clear();
        self.pending_points.clear();
        self.samples_per_tick = REFERENCE_SAMPLES_PER_TICK;

        // A trail drawn with the pen is left out on purpose: a pen shows where it is by
        // touching the screen, and the mouse pointer is not what its user looks at.
        self.update_drawing_cursor();

        // Whatever started this trail has normally recorded where, so the mouse pointer
        // is only a fallback. Starting from it every time put one stray point wherever
        // the mouse happened to sit: a hovering pen does not move the mouse pointer.
        if self.latest_position.is_none() {
            self.latest_position = cursor_position();
        }

        self.show();
        self.start_render_loop();
    }

    fn end_drawing(&mut self) {
        if !self.drawing {
            return;
        }
        self.drawing = false;
        // Keeps the pen shape while the mouse is still armed.
        self.update_drawing_cursor();
        // Let what has been drawn fade away instead of snapping off.
        self.stroke.finish();
    }

    /// The loop parked itself because the trail had faded while the pointer sat still.
    fn resume(&mut self) {
        if self.mouse_armed || self.pen_armed || self.always_on_button_held {
            self.show();
            self.start_render_loop();
        }
    }

    fn push_pending(&mut self, point: POINT, time_ms: u64) {
        if self.pending_points.len() >= MAX_PENDING_POINTS {
            self.pending_points.remove(0);
        }
        self.pending_points.push((point, time_ms));
    }

    // ---- Input ------------------------------------------------------------------

    /// Called for every mouse event in the system, so it has to be quick: anything
    /// that takes time is posted to the message loop instead. Returns whether the
    /// event should be kept from the application it was meant for.
    fn handle_mouse_input(&mut self, message: u32, event: &MSLLHOOKSTRUCT) -> bool {
        let from_pen = is_from_pen(event.dwExtraInfo);
        self.latest_position = Some(event.pt);

        // Positions made from the pen are not kept here: the pen is read from its
        // digitizer instead, which is both faster and more faithful.
        if self.drawing && !from_pen {
            self.push_pending(event.pt, now_ms());
        }
        if self.drawing && !self.render_loop_running {
            self.post(WM_WAKE, WAKE_RESUME);
        }

        let was_drawing = self.drawing;
        let mut swallow = false;

        if from_pen {
            if self.pen_armed {
                if self.settings.pen_render_when_close {
                    // Any sign of the pen counts as it being there, so the trail
                    // follows it without it touching.
                    self.pen_contact = message != WM_LBUTTONUP;
                } else if message == WM_LBUTTONDOWN {
                    self.pen_contact = true;
                } else if message == WM_LBUTTONUP {
                    self.pen_contact = false;
                }

                if self.pen_contact {
                    if !was_drawing && !self.pen_stroke_open {
                        // First position: the pointer is still travelling from where
                        // the mouse was, so it cannot be trusted. Skip it and start
                        // the trail from the next one.
                        self.pen_stroke_open = true;
                        self.pen_awaiting_first_sample = true;
                    } else {
                        self.pen_awaiting_first_sample = false;
                    }
                } else {
                    self.pen_stroke_open = false;
                    self.pen_awaiting_first_sample = false;
                }
                self.last_pen_input_ms = now_ms();

                // While armed the pen is a laser, not an input device. If its input
                // reached the application, scrollable content would take over the pen
                // and the trail would die in the middle of a stroke.
                swallow = true;
            }
        } else if let Some((button, is_down)) = classify_button(message, event.mouseData) {
            let may_swallow = self.settings.suppress_activation_button && button.is_spare();
            // The two ways of drawing are independent and may share a button, so each
            // is tracked separately.
            if self.mouse_armed && button == self.settings.activation_button {
                self.activation_button_held = is_down;
                swallow = may_swallow;
            }
            if self.settings.always_on_button == Some(button) {
                self.always_on_button_held = is_down;
                swallow = may_swallow;
            }
        }

        let should_draw =
            self.activation_button_held || self.always_on_button_held || self.pen_contact;
        if should_draw && !was_drawing {
            self.post(WM_WAKE, WAKE_BEGIN);
        } else if !should_draw && was_drawing {
            self.post(WM_WAKE, WAKE_END);
        }
        swallow
    }

    fn pen_present(&self, now: u64) -> bool {
        self.pen_armed
            && self.last_pen_input_ms != 0
            && now.saturating_sub(self.last_pen_input_ms) < PEN_RANGE_TIMEOUT_MS
    }

    /// Keeping pen input from applications through the mouse hook only works where
    /// Windows turns it into mouse messages. Over the taskbar and anything that scrolls
    /// by touch, the application claims the pen first, and the first contact scrolls it
    /// instead of drawing.
    ///
    /// Standing in front of it is the dependable answer: while the pen is armed and at
    /// the screen, the overlay stops letting input through around the pen, so its touch
    /// lands here and the application never sees it. Everywhere else, and whenever the
    /// pen is away, input passes through as usual.
    fn update_pen_capture(&mut self, now: u64) {
        let capture = self.pen_present(now);
        PEN_PATCH.set(capture.then_some(self.last_pen_point));
        if capture == self.pen_capturing {
            return;
        }
        self.pen_capturing = capture;

        // Answering hit tests is not enough: a transparent window is passed over before
        // it is ever asked, so the style has to come off for as long as this lasts.
        let transparent = WS_EX_TRANSPARENT.0 as isize;
        // SAFETY: the window belongs to this thread.
        unsafe {
            let style = GetWindowLongPtrW(self.window, GWL_EXSTYLE);
            let style = if capture {
                style & !transparent
            } else {
                style | transparent
            };
            SetWindowLongPtrW(self.window, GWL_EXSTYLE, style);
        }
    }

    fn handle_raw_input(&mut self, input: HRAWINPUT) {
        if !self.pen_armed {
            return;
        }
        let Some(report) = self.pen.read(input) else {
            return;
        };

        let want_draw = self.settings.pen_render_when_close || report.tip_down;
        let was_drawing = self.drawing;
        let now = now_ms();
        // Measured before the time is updated: a gap this long means the pen left and
        // came back somewhere else, and the positions either side must not be joined.
        let resumed_after_gap = self.last_pen_input_ms != 0
            && now.saturating_sub(self.last_pen_input_ms) > PEN_RANGE_TIMEOUT_MS;

        self.latest_position = Some(report.point);
        self.last_pen_point = report.point;
        self.last_pen_input_ms = now;

        // Reports arrive while the pen hovers, before it touches, and this is what
        // turns that into a window ready to catch the touch. Waiting for the next
        // frame would be too late: with nothing drawn, no frames are being made.
        self.update_pen_capture(now);
        if !self.render_loop_running {
            self.show();
            self.start_render_loop();
        }

        if want_draw {
            self.pen_contact = true;
            self.pen_awaiting_first_sample = false;
            if !was_drawing {
                self.begin_drawing();
            }
        } else if self.pen_contact {
            self.pen_contact = false;
            if was_drawing {
                self.end_drawing();
            }
        }

        // Every report is kept, not only the one that is current when a frame is drawn:
        // one position per frame made pen lines look angular next to the mouse's.
        // Starting a trail clears the list, so this has to come after that.
        if self.drawing && want_draw {
            if resumed_after_gap {
                self.pending_points.clear();
            }
            self.push_pending(report.point, now);
        }
    }

    // ---- Frames -----------------------------------------------------------------

    fn on_render_tick(&mut self) {
        let now = now_ms();

        // A hovering pen never says that it left, so silence is what ends its trail.
        // A touching pen does say when it lifts, and one held still against the screen
        // must not have its trail cut short.
        if self.pen_contact
            && self.settings.pen_render_when_close
            && now.saturating_sub(self.last_pen_input_ms) > PEN_IDLE_TIMEOUT_MS
        {
            self.pen_contact = false;
            if self.drawing && !self.activation_button_held && !self.always_on_button_held {
                self.end_drawing();
            }
        }

        if self.drawing && self.pen_awaiting_first_sample {
            // Wait for a position reported after the trail began.
            return;
        }

        if self.drawing
            && let Some(position) = self.latest_position
        {
            let scale = scale_at(position);

            // The trail tapers over a number of positions, so more positions per
            // second would shorten what is visible. Scaling the setting by the measured
            // rate keeps the trail the same length at any rate. It is smoothed because
            // the count per frame jitters, and never goes below the reference so a
            // slow mouse does not taper sooner than the setting says.
            let count = self.pending_points.len().max(1);
            self.samples_per_tick = self.samples_per_tick * 0.9 + count as f32 * 0.1;
            let rate_scale = self.samples_per_tick.max(REFERENCE_SAMPLES_PER_TICK);

            self.stroke.set_options(StrokeOptions {
                size: self.settings.size as f32 * scale,
                streamline: self.settings.streamline_percent as f32 / 100.0,
                decay_time_ms: self.settings.decay_time_ms,
                decay_length: (self.settings.decay_length as f32 * rate_scale) as u32,
                min_distance: 0.75 * scale,
            });

            if self.pending_points.is_empty() {
                // A pointer standing still: the head still needs a position so the
                // dot stays lit.
                self.stroke
                    .add_point(position.x as f32, position.y as f32, now);
            } else {
                for (point, time_ms) in self.pending_points.drain(..) {
                    self.stroke
                        .add_point(point.x as f32, point.y as f32, time_ms);
                }
            }
        }

        // A pen at the screen keeps the overlay up even with nothing drawn: a hidden
        // window is never asked about input, and catching the pen is the point.
        let pen_present = self.pen_present(now);
        self.update_pen_capture(now);

        let hone_running = self.hone_running(now);
        if !hone_running {
            self.hone = None;
        }

        let mut has_content = self.stroke.prune(now) || pen_present || hone_running;
        self.past_strokes.retain_mut(|stroke| stroke.prune(now));
        has_content |= !self.past_strokes.is_empty();

        if !has_content {
            // Nothing left to draw. Blank the overlay once and then go idle, so no
            // timer runs and no frames are made while there is nothing to show.
            // Movement wakes the loop again from the mouse hook.
            self.render(now);
            self.stop_render_loop();
            self.hide();
            self.stroke.clear();
            return;
        }

        if !self.visible {
            self.show();
        }
        // The device may have been lost during the previous frame.
        if self.graphics.is_none() && !self.ensure_geometry() {
            return;
        }
        self.render(now);
    }

    // ---- Settings ---------------------------------------------------------------

    fn process_pending_settings(&mut self) {
        let pending = self
            .shared
            .pending_settings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(settings) = pending else { return };

        let previous = std::mem::replace(&mut self.settings, settings);
        if self.settings.always_on_button != previous.always_on_button {
            // Changing the button in the middle of a press would leave the flag set
            // for good: the release that clears it no longer matches the setting.
            self.always_on_button_held = false;
        }
        // The always-on button decides whether the hook runs while nothing is armed.
        self.update_hook();

        if self.settings.activation_shortcut != previous.activation_shortcut
            || self.settings.pen_activation_shortcut != previous.pen_activation_shortcut
        {
            self.update_hotkeys();
        }
        // Without its shortcut the pen could not be disarmed again.
        if self.settings.pen_activation_shortcut.is_none() {
            self.disarm_pen();
        }
    }

    fn unregister_hotkeys(&self) {
        for id in [HOTKEY_MOUSE, HOTKEY_PEN] {
            // SAFETY: the window belongs to this thread; an id that was never
            // registered only makes the call fail.
            let _ = unsafe { UnregisterHotKey(Some(self.window), id) };
        }
    }

    fn update_hotkeys(&self) {
        self.unregister_hotkeys();
        for (id, shortcut, purpose) in [
            (HOTKEY_MOUSE, self.settings.activation_shortcut, "mouse"),
            (HOTKEY_PEN, self.settings.pen_activation_shortcut, "pen"),
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
                Ok(()) => log::info!("laser pointer {purpose} shortcut is {shortcut}"),
                Err(error) => log::error!(
                    "could not register {shortcut} as the {purpose} shortcut, which usually \
                     means something else is using it: {error}"
                ),
            }
        }
    }
}

fn hone_active(hone: &Hone, now: u64) -> bool {
    now.saturating_sub(hone.start_ms) < HONE_DURATION_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_input_is_told_from_mouse_and_touch() {
        assert!(is_from_pen(0xFF51_5700));
        assert!(is_from_pen(0xFF51_5712));
        // The same signature with the touch bit set.
        assert!(!is_from_pen(0xFF51_5780));
        assert!(!is_from_pen(0));
        assert!(!is_from_pen(0x1234_5600));
    }

    #[test]
    fn button_messages_name_their_button() {
        assert_eq!(
            classify_button(WM_MBUTTONDOWN, 0),
            Some((MouseButton::Middle, true))
        );
        assert_eq!(
            classify_button(WM_LBUTTONUP, 0),
            Some((MouseButton::Left, false))
        );
        assert_eq!(
            classify_button(WM_XBUTTONDOWN, 1 << 16),
            Some((MouseButton::X1, true))
        );
        assert_eq!(
            classify_button(WM_XBUTTONUP, 2 << 16),
            Some((MouseButton::X2, false))
        );
    }

    #[test]
    fn movement_and_the_wheel_are_not_buttons() {
        use windows::Win32::UI::WindowsAndMessaging::{WM_MOUSEMOVE, WM_MOUSEWHEEL};

        assert_eq!(classify_button(WM_MOUSEMOVE, 0), None);
        assert_eq!(classify_button(WM_MOUSEWHEEL, 120 << 16), None);
    }

    #[test]
    fn a_shortcut_becomes_its_modifiers_without_repeat() {
        let shortcut = Shortcut::parse("Win+Shift+L").unwrap();

        assert_eq!(
            hotkey_modifiers(&shortcut),
            MOD_WIN | MOD_SHIFT | MOD_NOREPEAT
        );
    }

    #[test]
    fn the_overlay_starts_and_stops() {
        let overlay = OverlayThread::start(
            LaserPointerSettings {
                // Tests must not take a shortcut away from a running copy of the app.
                activation_shortcut: None,
                ..LaserPointerSettings::default()
            },
            Annotations::new(),
        )
        .unwrap();

        overlay.apply_settings(LaserPointerSettings {
            activation_shortcut: None,
            size: 20,
            ..LaserPointerSettings::default()
        });
        drop(overlay);
    }
}
