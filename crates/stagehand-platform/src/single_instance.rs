//! Keeping Stagehand to one running process per signed-in user.
//!
//! The first process to start owns a named event. A later process finds the event
//! already there, can signal it to ask the first one to come forward, and exits.

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::Arc;
use std::thread;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
use windows::core::HSTRING;

pub enum Instance {
    /// No other instance is running; this process is the one that stays.
    Primary(PrimaryInstance),
    /// Another instance is already running.
    Secondary(SecondaryInstance),
}

pub struct PrimaryInstance {
    event: Arc<OwnedHandle>,
}

pub struct SecondaryInstance {
    event: OwnedHandle,
}

/// Claims the single instance slot for `app_id`, or finds out that it is taken.
///
/// The slot is per sign-in session, so two users signed in to the same machine each get
/// their own Stagehand.
pub fn acquire(app_id: &str) -> io::Result<Instance> {
    let name = HSTRING::from(format!(r"Local\{app_id}.SingleInstance"));
    // SAFETY: `name` is a valid, nul-terminated wide string that outlives the call, and
    // the security attributes are allowed to be absent.
    let handle = unsafe { CreateEventW(None, false, false, &name) }.map_err(io::Error::from)?;
    // SAFETY: reading the calling thread's last error has no preconditions. It still
    // holds the result of CreateEventW, which sets ERROR_ALREADY_EXISTS when it opened
    // an existing event instead of creating one.
    let already_exists = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: `handle` was just returned by CreateEventW, is valid, and is owned by
    // nothing else.
    let event = unsafe { OwnedHandle::from_raw_handle(handle.0) };

    Ok(if already_exists {
        Instance::Secondary(SecondaryInstance { event })
    } else {
        Instance::Primary(PrimaryInstance {
            event: Arc::new(event),
        })
    })
}

impl PrimaryInstance {
    /// Calls `on_activate` every time another instance asks this one to come forward.
    ///
    /// The callback runs on a background thread that lives as long as the process.
    pub fn on_activate(&self, on_activate: impl Fn() + Send + 'static) -> io::Result<()> {
        // The thread shares ownership of the event so the handle stays open for as long
        // as it is waiting on it, whatever happens to `self`.
        let event = Arc::clone(&self.event);
        thread::Builder::new()
            .name("single-instance".to_owned())
            .spawn(move || {
                loop {
                    // SAFETY: `event` keeps the handle open for the duration of the wait.
                    let result = unsafe { WaitForSingleObject(raw(&event), INFINITE) };
                    if result != WAIT_OBJECT_0 {
                        break;
                    }
                    on_activate();
                }
            })?;
        Ok(())
    }
}

impl SecondaryInstance {
    /// Asks the instance that is already running to come forward.
    pub fn activate_primary(&self) -> io::Result<()> {
        // Windows only lets the process the user just started take the foreground.
        // That is this one, so it hands the right on; otherwise the running instance's
        // window would only flash in the taskbar. Failing to is harmless.
        // SAFETY: the call takes no pointers and has no preconditions.
        let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
        // SAFETY: `self.event` is an open event handle.
        unsafe { SetEvent(raw(&self.event)) }.map_err(io::Error::from)
    }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn unique_id(test: &str) -> String {
        format!("Stagehand.Tests.{test}.{}", std::process::id())
    }

    #[test]
    fn second_acquire_is_secondary_and_can_activate_the_first() {
        let id = unique_id("activate");
        let Instance::Primary(primary) = acquire(&id).unwrap() else {
            panic!("the first instance should be the primary one");
        };
        let (sender, receiver) = mpsc::channel();
        primary
            .on_activate(move || {
                let _ = sender.send(());
            })
            .unwrap();

        let Instance::Secondary(secondary) = acquire(&id).unwrap() else {
            panic!("the second instance should be secondary");
        };
        secondary.activate_primary().unwrap();

        receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the primary instance should have been activated");
    }

    #[test]
    fn a_secondary_that_does_not_activate_leaves_the_primary_alone() {
        let id = unique_id("quiet");
        let Instance::Primary(primary) = acquire(&id).unwrap() else {
            panic!("the first instance should be the primary one");
        };
        let (sender, receiver) = mpsc::channel();
        primary
            .on_activate(move || {
                let _ = sender.send(());
            })
            .unwrap();

        let second = acquire(&id).unwrap();
        assert!(matches!(second, Instance::Secondary(_)));
        drop(second);

        assert!(receiver.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn different_ids_do_not_see_each_other() {
        let first = acquire(&unique_id("one")).unwrap();
        let second = acquire(&unique_id("two")).unwrap();

        assert!(matches!(first, Instance::Primary(_)));
        assert!(matches!(second, Instance::Primary(_)));
    }
}
