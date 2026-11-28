use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use stagehand_core::{Plugin, PluginError, PluginInfo};

use crate::posture::PostureWatcher;
use crate::settings::{Posture, TaskbarSwitchSettings, Trigger};
use crate::{ID, placement};

#[derive(Default)]
pub struct TaskbarSwitch {
    // Written when the settings change, read on the watcher thread.
    settings: Arc<Mutex<TaskbarSwitchSettings>>,
    watcher: Option<PostureWatcher>,
}

impl TaskbarSwitch {
    pub fn new() -> Self {
        Self::default()
    }

    fn store(&self, settings: &Value) {
        let settings = TaskbarSwitchSettings::from_value(settings);
        *self.settings.lock().unwrap_or_else(PoisonError::into_inner) = settings;
    }
}

impl Plugin for TaskbarSwitch {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: ID,
            name: "Taskbar Switch",
            description: "Moves the taskbar to another edge of the screen when you switch \
                          between using the keyboard and using the device as a tablet.",
            // Moving the taskbar is too visible a change to make before being asked.
            enabled_by_default: false,
        }
    }

    fn is_supported(&self) -> bool {
        placement::is_available()
    }

    fn start(&mut self, settings: &Value) -> Result<(), PluginError> {
        self.store(settings);
        let settings = Arc::clone(&self.settings);
        self.watcher = Some(PostureWatcher::start(move |posture, trigger| {
            let settings = *settings.lock().unwrap_or_else(PoisonError::into_inner);
            place_taskbar(&settings, posture, trigger)
        })?);
        Ok(())
    }

    fn stop(&mut self) {
        self.watcher = None;
    }

    fn apply_settings(&mut self, settings: &Value) -> Result<(), PluginError> {
        self.store(settings);
        if let Some(watcher) = &self.watcher {
            watcher.request_reapply();
        }
        Ok(())
    }
}

/// Runs on the watcher thread, which is the one set up to talk to the shell. Returns
/// whether the taskbar is known to be where it should be, which it is not right after
/// it was moved: the shell moves it in its own time.
fn place_taskbar(settings: &TaskbarSwitchSettings, posture: Posture, trigger: Trigger) -> bool {
    let Some(edge) = settings.target_edge(posture, trigger) else {
        if trigger == Trigger::Started {
            log::info!("leaving the taskbar where it is at start, as the settings say");
        }
        return true;
    };
    // Only the placement made at the start is checked against the taskbar itself and
    // made again: that is the one the shell may have been too busy to act on.
    let at_start = trigger == Trigger::Started;
    match placement::apply(edge, at_start) {
        Ok(true) => {
            log::info!("moved the taskbar to the {}", edge.name());
            false
        }
        Ok(false) if at_start => {
            let confirmed = placement::is_confirmed_on(edge);
            if confirmed {
                log::info!("the taskbar is on the {}, where it should be", edge.name());
            }
            confirmed
        }
        Ok(false) => true,
        Err(error) => {
            log::error!("could not move the taskbar to the {}: {error}", edge.name());
            false
        }
    }
}
