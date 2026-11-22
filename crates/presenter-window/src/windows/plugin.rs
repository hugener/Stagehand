use serde_json::Value;
use stagehand_core::{Annotations, Plugin, PluginError, PluginInfo};
use windows::Graphics::Capture::GraphicsCaptureSession;

use super::presenter::PresenterThread;
use crate::settings::PresenterWindowSettings;
use crate::{ACTION_SHARE, ACTION_STOP, ID};

pub struct PresenterWindow {
    annotations: Annotations,
    presenter: Option<PresenterThread>,
}

impl PresenterWindow {
    /// `annotations` is what the other tools draw on the screen, which is added to the
    /// mirror.
    pub fn new(annotations: Annotations) -> Self {
        Self {
            annotations,
            presenter: None,
        }
    }
}

impl Plugin for PresenterWindow {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: ID,
            name: "Presenter Window",
            description: "Mirrors a window into one you can share in a call, with the laser \
                          trail on it, so viewers see it without you sharing the whole screen.",
            // It does nothing until its shortcut is pressed.
            enabled_by_default: true,
        }
    }

    fn is_supported(&self) -> bool {
        GraphicsCaptureSession::IsSupported().unwrap_or(false)
    }

    fn start(&mut self, settings: &Value) -> Result<(), PluginError> {
        let settings = PresenterWindowSettings::from_value(settings);
        self.presenter = Some(PresenterThread::start(settings, self.annotations.clone())?);
        Ok(())
    }

    fn stop(&mut self) {
        self.presenter = None;
    }

    fn apply_settings(&mut self, settings: &Value) -> Result<(), PluginError> {
        if let Some(presenter) = &self.presenter {
            presenter.apply_settings(PresenterWindowSettings::from_value(settings));
        }
        Ok(())
    }

    fn invoke(&mut self, action: &str) {
        let Some(presenter) = &self.presenter else {
            return;
        };
        match action {
            ACTION_SHARE => presenter.share_previous_window(),
            ACTION_STOP => presenter.stop_sharing(),
            _ => {}
        }
    }
}
