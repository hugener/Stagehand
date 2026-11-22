use serde_json::Value;
use stagehand_core::{Annotations, Plugin, PluginError, PluginInfo};

use super::overlay::OverlayThread;
use crate::ID;
use crate::settings::LaserPointerSettings;

pub struct LaserPointer {
    annotations: Annotations,
    overlay: Option<OverlayThread>,
}

impl LaserPointer {
    /// `annotations` is where the trails are published for tools that show them to
    /// remote viewers.
    pub fn new(annotations: Annotations) -> Self {
        Self {
            annotations,
            overlay: None,
        }
    }
}

impl Plugin for LaserPointer {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            id: ID,
            name: "Laser Pointer",
            description: "Draws a fading trail on the screen with the mouse or a pen, for \
                          pointing things out while presenting.",
            // It does nothing until its shortcut is pressed.
            enabled_by_default: true,
        }
    }

    fn start(&mut self, settings: &Value) -> Result<(), PluginError> {
        let settings = LaserPointerSettings::from_value(settings);
        self.overlay = Some(OverlayThread::start(settings, self.annotations.clone())?);
        Ok(())
    }

    fn stop(&mut self) {
        self.overlay = None;
    }

    fn apply_settings(&mut self, settings: &Value) -> Result<(), PluginError> {
        if let Some(overlay) = &self.overlay {
            overlay.apply_settings(LaserPointerSettings::from_value(settings));
        }
        Ok(())
    }
}
