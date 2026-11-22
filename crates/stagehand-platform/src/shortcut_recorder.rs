//! Listens for the keys of a keyboard shortcut the user is choosing.
//!
//! While a [`Recorder`] is alive, every key pressed in one of Stagehand's own windows
//! goes to it instead of to the window or to Windows. That is what lets a shortcut with
//! the Windows key, or one that is already in use, be pressed without it also doing
//! what it normally does.

use std::cell::RefCell;
use std::io;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HC_ACTION,
    KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

/// The virtual key code of the Escape key, which by convention cancels recording.
pub const ESCAPE: u32 = 0x1B;

/// The modifier keys held down at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Modifiers {
    pub fn any(self) -> bool {
        self.win || self.ctrl || self.alt || self.shift
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A modifier key went down or up; these are the ones held now.
    Held(Modifiers),
    /// A key that is not a modifier went down, with these modifiers held.
    Pressed {
        modifiers: Modifiers,
        /// The key as a Windows virtual key code.
        virtual_key: u32,
    },
}

#[derive(Clone, Copy)]
enum Modifier {
    Win,
    Ctrl,
    Alt,
    Shift,
}

/// Every virtual key code that is a modifier. Each physical key is followed on its own,
/// so that letting go of one Shift while the other is still down leaves Shift held.
const MODIFIER_KEYS: [(u32, Modifier); 11] = [
    (0x5B, Modifier::Win),   // left Windows key
    (0x5C, Modifier::Win),   // right Windows key
    (0x10, Modifier::Shift), // either Shift, as some programs send it
    (0xA0, Modifier::Shift), // left Shift
    (0xA1, Modifier::Shift), // right Shift
    (0x11, Modifier::Ctrl),  // either Ctrl
    (0xA2, Modifier::Ctrl),  // left Ctrl
    (0xA3, Modifier::Ctrl),  // right Ctrl
    (0x12, Modifier::Alt),   // either Alt
    (0xA4, Modifier::Alt),   // left Alt
    (0xA5, Modifier::Alt),   // right Alt
];

/// Turns key presses and releases into [`Event`]s.
#[derive(Default)]
struct Tracker {
    // One bit for each entry of `MODIFIER_KEYS` that is down.
    down: u16,
}

impl Tracker {
    fn modifiers(&self) -> Modifiers {
        let mut modifiers = Modifiers::default();
        for (index, (_, modifier)) in MODIFIER_KEYS.iter().enumerate() {
            if self.down & (1 << index) != 0 {
                match modifier {
                    Modifier::Win => modifiers.win = true,
                    Modifier::Ctrl => modifiers.ctrl = true,
                    Modifier::Alt => modifiers.alt = true,
                    Modifier::Shift => modifiers.shift = true,
                }
            }
        }
        modifiers
    }

    fn key(&mut self, virtual_key: u32, down: bool) -> Option<Event> {
        let modifier = MODIFIER_KEYS
            .iter()
            .position(|(key, _)| *key == virtual_key);
        match modifier {
            Some(index) => {
                let before = self.modifiers();
                if down {
                    self.down |= 1 << index;
                } else {
                    self.down &= !(1 << index);
                }
                let now = self.modifiers();
                (now != before).then_some(Event::Held(now))
            }
            None if down => Some(Event::Pressed {
                modifiers: self.modifiers(),
                virtual_key,
            }),
            None => None,
        }
    }

    fn release_all(&mut self) {
        self.down = 0;
    }
}

struct State {
    tracker: Tracker,
    handler: Box<dyn Fn(Event)>,
}

thread_local! {
    // The hook procedure is called on the thread that installed the hook and is given
    // nothing to find its recorder by, so the state of that thread is kept here.
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Records for as long as it is alive.
pub struct Recorder {
    thread: Option<JoinHandle<()>>,
    thread_id: u32,
}

impl Recorder {
    /// Starts recording. `handler` is called on a thread of the recorder's own, in the
    /// middle of the system handing over a key, so it should do no more than pass the
    /// event on.
    pub fn start(handler: impl Fn(Event) + Send + 'static) -> io::Result<Self> {
        // The keys are listened for on a thread that does nothing else, so that they
        // are never kept waiting behind the window being drawn.
        let (started, start) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("shortcut-recorder".to_owned())
            .spawn(move || listen(Box::new(handler), &started))?;
        match start.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                thread: Some(thread),
                thread_id,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::other(
                "the shortcut recorder stopped unexpectedly",
            )),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // SAFETY: posting to a thread has no preconditions; if the thread is already
        // gone the call fails and there is nothing left to stop.
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The recorder's thread: installs the hook, reports how that went and then handles
/// messages, which is when the hook is called, until it is told to quit.
fn listen(handler: Box<dyn Fn(Event)>, started: &mpsc::Sender<io::Result<u32>>) {
    let mut message = MSG::default();
    // SAFETY: looking at the message queue creates it, so that the message that stops
    // this thread cannot be posted before there is a queue to take it.
    let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE) };

    // SAFETY: `hook_procedure` has the signature a low-level keyboard hook needs and,
    // being a function, stays valid for as long as the hook is installed.
    let hook = unsafe {
        GetModuleHandleW(None).and_then(|module| {
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_procedure), Some(module.into()), 0)
        })
    };
    let hook = match hook {
        Ok(hook) => hook,
        Err(error) => {
            let _ = started.send(Err(error.into()));
            return;
        }
    };
    STATE.with_borrow_mut(|state| {
        *state = Some(State {
            tracker: Tracker::default(),
            handler,
        });
    });
    // SAFETY: asking for the id of the calling thread has no preconditions.
    let _ = started.send(Ok(unsafe { GetCurrentThreadId() }));

    // SAFETY: `message` is a valid place for the next message for as long as the loop
    // runs. The hook is called from inside `GetMessageW`.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {}

    // SAFETY: the hook was installed above and is removed only here.
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    STATE.with_borrow_mut(Option::take);
}

/// Whether the window the user is typing in is one of Stagehand's.
fn typing_in_own_window() -> bool {
    let mut process = 0u32;
    // SAFETY: both calls only read; a window that has just gone leaves `process` at 0.
    unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut process));
        process == GetCurrentProcessId()
    }
}

unsafe extern "system" fn hook_procedure(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let swallow = code == HC_ACTION as i32
        && STATE.with(|state| {
            let Ok(mut state) = state.try_borrow_mut() else {
                return false;
            };
            let Some(state) = state.as_mut() else {
                return false;
            };
            if !typing_in_own_window() {
                // Keys typed elsewhere are none of the recorder's business, and their
                // releases will not be seen here either.
                state.tracker.release_all();
                return false;
            }
            // SAFETY: for `HC_ACTION` Windows passes a pointer to a `KBDLLHOOKSTRUCT`
            // that is valid for the length of this call.
            let key = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            if let Some(event) = state.tracker.key(key.vkCode, down) {
                (state.handler)(event);
            }
            true
        });
    if swallow {
        // The key is part of the shortcut being chosen, not something to act on.
        LRESULT(1)
    } else {
        // SAFETY: passing on exactly what was received, as the hook contract asks.
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT_WIN: u32 = 0x5B;
    const LEFT_SHIFT: u32 = 0xA0;
    const RIGHT_SHIFT: u32 = 0xA1;
    const LEFT_CTRL: u32 = 0xA2;
    const RIGHT_ALT: u32 = 0xA5;

    #[test]
    fn modifiers_are_reported_as_they_go_down_and_up() {
        let mut tracker = Tracker::default();

        assert_eq!(
            tracker.key(LEFT_WIN, true),
            Some(Event::Held(Modifiers {
                win: true,
                ..Modifiers::default()
            }))
        );
        assert_eq!(
            tracker.key(LEFT_SHIFT, true),
            Some(Event::Held(Modifiers {
                win: true,
                shift: true,
                ..Modifiers::default()
            }))
        );
        assert_eq!(
            tracker.key(LEFT_WIN, false),
            Some(Event::Held(Modifiers {
                shift: true,
                ..Modifiers::default()
            }))
        );
    }

    #[test]
    fn a_key_is_reported_with_the_modifiers_held() {
        let mut tracker = Tracker::default();
        tracker.key(LEFT_CTRL, true);
        tracker.key(RIGHT_ALT, true);

        assert_eq!(
            tracker.key(u32::from(b'L'), true),
            Some(Event::Pressed {
                modifiers: Modifiers {
                    ctrl: true,
                    alt: true,
                    ..Modifiers::default()
                },
                virtual_key: u32::from(b'L'),
            })
        );
        assert_eq!(tracker.key(u32::from(b'L'), false), None);
    }

    #[test]
    fn a_modifier_stays_held_while_its_twin_is_still_down() {
        let mut tracker = Tracker::default();
        tracker.key(LEFT_SHIFT, true);

        // Nothing changed for the listener: Shift was held and still is.
        assert_eq!(tracker.key(RIGHT_SHIFT, true), None);
        assert_eq!(tracker.key(LEFT_SHIFT, false), None);
        assert_eq!(
            tracker.key(RIGHT_SHIFT, false),
            Some(Event::Held(Modifiers::default()))
        );
    }

    #[test]
    fn a_held_modifier_repeating_is_not_reported_again() {
        let mut tracker = Tracker::default();
        tracker.key(LEFT_CTRL, true);

        assert_eq!(tracker.key(LEFT_CTRL, true), None);
    }

    #[test]
    fn nothing_is_held_after_everything_is_released() {
        let mut tracker = Tracker::default();
        tracker.key(LEFT_WIN, true);
        tracker.key(LEFT_SHIFT, true);

        tracker.release_all();

        assert!(!tracker.modifiers().any());
    }
}
