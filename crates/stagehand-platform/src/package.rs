//! Whether Stagehand runs from an installed package, as it does when it comes from the
//! Microsoft Store, and what that changes.
//!
//! A packaged app is still an ordinary desktop program, but a few things are done
//! through the package instead of directly: starting at sign-in is one of them.

use std::io;

use windows::ApplicationModel::Activation::ActivationKind;
use windows::ApplicationModel::{AppInstance, StartupTask, StartupTaskState};
use windows::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
use windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
use windows::core::h;

/// The startup task declared in the package manifest. The two have to match.
const STARTUP_TASK_ID: &windows::core::HSTRING = h!("StagehandStartup");

pub fn is_packaged() -> bool {
    let mut length = 0u32;
    // SAFETY: with no buffer the call only reports whether there is a package and how
    // long its name is.
    unsafe { GetCurrentPackageFullName(&mut length, None) != APPMODEL_ERROR_NO_PACKAGE }
}

/// Whether this start is the package's startup task running at sign-in, as opposed to
/// the user opening the app. Only meaningful when packaged; a startup task cannot be
/// given a command line to tell the two apart by.
pub fn started_at_sign_in() -> bool {
    is_packaged()
        && AppInstance::GetActivatedEventArgs()
            .and_then(|arguments| arguments.Kind())
            .is_ok_and(|kind| kind == ActivationKind::StartupTask)
}

fn startup_task() -> io::Result<StartupTask> {
    Ok(StartupTask::GetAsync(STARTUP_TASK_ID)?.join()?)
}

fn is_on(state: StartupTaskState) -> bool {
    state == StartupTaskState::Enabled || state == StartupTaskState::EnabledByPolicy
}

/// Why a startup task in `state` cannot be switched on from here, if it cannot.
fn refusal(state: StartupTaskState) -> Option<&'static str> {
    if state == StartupTaskState::DisabledByUser {
        // Windows keeps the user's own choice and lets nobody else overrule it.
        Some(
            "starting at sign-in was turned off in Windows; turn it back on under \
             Settings > Apps > Startup",
        )
    } else if state == StartupTaskState::DisabledByPolicy {
        Some("starting at sign-in is turned off by your organisation")
    } else {
        None
    }
}

/// What Windows says about starting at sign-in, in words for the log. `None` when not
/// packaged, where there is no startup task to ask about.
pub fn startup_task_state() -> Option<String> {
    if !is_packaged() {
        return None;
    }
    Some(match startup_task().and_then(|task| Ok(task.State()?)) {
        Ok(state) => state_name(state).to_owned(),
        Err(error) => format!("unknown ({error})"),
    })
}

fn state_name(state: StartupTaskState) -> &'static str {
    match state {
        StartupTaskState::Disabled => "off",
        StartupTaskState::DisabledByUser => "turned off in Windows",
        StartupTaskState::Enabled => "on",
        StartupTaskState::DisabledByPolicy => "turned off by policy",
        StartupTaskState::EnabledByPolicy => "turned on by policy",
        _ => "in a state this version does not know",
    }
}

pub(crate) fn startup_task_enabled() -> bool {
    startup_task()
        .and_then(|task| Ok(task.State()?))
        .is_ok_and(is_on)
}

pub(crate) fn set_startup_task_enabled(enabled: bool) -> io::Result<()> {
    let task = startup_task()?;
    if !enabled {
        return Ok(task.Disable()?);
    }
    let state = task.RequestEnableAsync()?.join()?;
    match refusal(state) {
        Some(reason) => Err(io::Error::other(reason)),
        None if is_on(state) => Ok(()),
        None => Err(io::Error::other(
            "Windows did not turn on starting at sign-in",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_do_not_run_from_a_package() {
        assert!(!is_packaged());
        assert!(!started_at_sign_in());
    }

    #[test]
    fn only_enabled_states_count_as_on() {
        assert!(is_on(StartupTaskState::Enabled));
        assert!(is_on(StartupTaskState::EnabledByPolicy));
        assert!(!is_on(StartupTaskState::Disabled));
        assert!(!is_on(StartupTaskState::DisabledByUser));
        assert!(!is_on(StartupTaskState::DisabledByPolicy));
    }

    #[test]
    fn there_is_no_startup_task_to_describe_outside_a_package() {
        assert_eq!(startup_task_state(), None);
        assert_eq!(state_name(StartupTaskState::Enabled), "on");
        assert_eq!(
            state_name(StartupTaskState::DisabledByUser),
            "turned off in Windows"
        );
    }

    #[test]
    fn a_choice_made_in_windows_cannot_be_overruled() {
        assert!(refusal(StartupTaskState::DisabledByUser).is_some());
        assert!(refusal(StartupTaskState::DisabledByPolicy).is_some());
        assert!(refusal(StartupTaskState::Disabled).is_none());
        assert!(refusal(StartupTaskState::Enabled).is_none());
    }
}
