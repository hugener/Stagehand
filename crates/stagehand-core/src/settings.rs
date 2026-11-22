use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::PluginInfo;

/// Everything Stagehand stores for the user.
///
/// Whether Stagehand starts at sign-in is deliberately not in here: the operating system
/// owns that switch and the user can flip it outside the app, so it is always read from
/// the system.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    plugins: BTreeMap<String, PluginSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PluginSettings {
    /// `None` until the user switches the plugin on or off, so that a plugin's own
    /// default applies until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Owned by the plugin; the host only stores it.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub settings: Value,
}

impl Settings {
    pub fn plugin_enabled(&self, info: &PluginInfo) -> bool {
        self.plugins
            .get(info.id)
            .and_then(|plugin| plugin.enabled)
            .unwrap_or(info.enabled_by_default)
    }

    pub fn set_plugin_enabled(&mut self, id: &str, enabled: bool) {
        self.plugins.entry(id.to_owned()).or_default().enabled = Some(enabled);
    }

    /// The plugin's own settings, or `Value::Null` when it has none stored.
    pub fn plugin_settings(&self, id: &str) -> &Value {
        self.plugins
            .get(id)
            .map_or(&Value::Null, |plugin| &plugin.settings)
    }

    pub fn set_plugin_settings(&mut self, id: &str, settings: Value) {
        self.plugins.entry(id.to_owned()).or_default().settings = settings;
    }
}

/// Reads and writes the settings file.
#[derive(Debug, Clone)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the settings. A missing file gives the defaults. A file that cannot be
    /// parsed is moved aside to `<name>.bad`, so the user's edits are not lost, and the
    /// defaults are returned.
    pub fn load(&self) -> io::Result<Settings> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
            Err(error) => return Err(error),
        };
        match serde_json::from_str(&text) {
            Ok(settings) => Ok(settings),
            Err(_) => {
                fs::rename(&self.path, self.sibling("bad"))?;
                Ok(Settings::default())
            }
        }
    }

    /// Writes the settings through a temporary file, so a crash mid-write cannot leave a
    /// half-written settings file behind.
    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        if let Some(directory) = self.path.parent() {
            fs::create_dir_all(directory)?;
        }
        let text = serde_json::to_string_pretty(settings).map_err(io::Error::other)?;
        let temporary = self.sibling("tmp");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, &self.path)
    }

    fn sibling(&self, extension: &str) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(".");
        name.push(extension);
        PathBuf::from(name)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;

    /// A directory under the system temp folder that is removed when dropped.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let name = format!(
                "stagehand-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(name);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const INFO: PluginInfo = PluginInfo {
        id: "laser",
        name: "Laser",
        description: "",
        enabled_by_default: true,
    };

    #[test]
    fn plugin_default_applies_until_the_user_chooses() {
        let mut settings = Settings::default();
        let off_by_default = PluginInfo {
            enabled_by_default: false,
            ..INFO
        };
        assert!(settings.plugin_enabled(&INFO));
        assert!(!settings.plugin_enabled(&off_by_default));

        settings.set_plugin_enabled("laser", false);
        assert!(!settings.plugin_enabled(&INFO));

        settings.set_plugin_enabled("laser", true);
        assert!(settings.plugin_enabled(&off_by_default));
    }

    #[test]
    fn storing_plugin_settings_does_not_decide_whether_it_is_enabled() {
        let mut settings = Settings::default();
        settings.set_plugin_settings("laser", json!({ "size": 12 }));

        assert_eq!(settings.plugin_settings("laser"), &json!({ "size": 12 }));
        assert_eq!(settings.plugin_settings("other"), &Value::Null);
        assert!(settings.plugin_enabled(&INFO));
    }

    #[test]
    fn missing_file_loads_defaults() {
        let directory = TempDir::new();
        let store = SettingsStore::new(directory.0.join("settings.json"));

        assert_eq!(store.load().unwrap(), Settings::default());
    }

    #[test]
    fn save_creates_the_directory_and_round_trips() {
        let directory = TempDir::new();
        let store = SettingsStore::new(directory.0.join("nested").join("settings.json"));
        let mut settings = Settings::default();
        settings.set_plugin_enabled("laser", false);
        settings.set_plugin_settings("laser", json!({ "size": 12 }));

        store.save(&settings).unwrap();

        assert_eq!(store.load().unwrap(), settings);
        assert!(
            !directory
                .0
                .join("nested")
                .join("settings.json.tmp")
                .exists()
        );
    }

    #[test]
    fn unparseable_file_is_moved_aside_and_defaults_are_loaded() {
        let directory = TempDir::new();
        let path = directory.0.join("settings.json");
        fs::write(&path, "{ not json").unwrap();
        let store = SettingsStore::new(&path);

        assert_eq!(store.load().unwrap(), Settings::default());
        assert!(!path.exists());
        assert_eq!(
            fs::read_to_string(directory.0.join("settings.json.bad")).unwrap(),
            "{ not json"
        );
    }

    #[test]
    fn unknown_fields_and_missing_sections_are_tolerated() {
        let directory = TempDir::new();
        let path = directory.0.join("settings.json");
        fs::write(&path, r#"{ "future": 1, "plugins": { "laser": {} } }"#).unwrap();

        let settings = SettingsStore::new(&path).load().unwrap();

        assert!(settings.plugin_enabled(&INFO));
        assert_eq!(settings.plugin_settings("laser"), &Value::Null);
    }
}
