use std::fmt;

use serde_json::Value;

use crate::Settings;

/// What a plugin reports when it cannot start or cannot take new settings.
pub type PluginError = Box<dyn std::error::Error + Send + Sync>;

/// Fixed facts about a plugin, shown in the settings window and used as keys in the
/// settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginInfo {
    /// Key in the settings file. Never change it once released: doing so orphans the
    /// user's settings for the plugin.
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Whether the plugin runs for a user who has never switched it on or off.
    pub enabled_by_default: bool,
}

/// A tool hosted by Stagehand.
///
/// Plugins are ordinary crates compiled into the application. All methods are called on
/// the application's main thread.
pub trait Plugin {
    fn info(&self) -> PluginInfo;

    /// Whether the plugin can work on this machine at all. A plugin that returns false
    /// is listed as unavailable and never started.
    fn is_supported(&self) -> bool {
        true
    }

    /// Starts the plugin with its stored settings. `settings` is `Value::Null` when the
    /// user has none yet, which every plugin must treat as "use the defaults".
    fn start(&mut self, settings: &Value) -> Result<(), PluginError>;

    /// Stops the plugin and releases everything `start` acquired. Only called after a
    /// successful `start`.
    fn stop(&mut self);

    /// Applies changed settings to a running plugin.
    fn apply_settings(&mut self, settings: &Value) -> Result<(), PluginError>;

    /// Does one of the things the plugin can be asked to do from outside its own
    /// shortcuts, such as from the tray menu. The names are the plugin's own; one it
    /// does not know is ignored. Only called while the plugin is running.
    fn invoke(&mut self, action: &str) {
        let _ = action;
    }
}

/// A plugin together with what the host currently knows about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginStatus {
    pub info: PluginInfo,
    pub supported: bool,
    pub running: bool,
}

#[derive(Debug)]
pub enum HostError {
    UnknownPlugin(String),
    Unsupported(&'static str),
    Plugin {
        id: &'static str,
        source: PluginError,
    },
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlugin(id) => write!(f, "there is no plugin called '{id}'"),
            Self::Unsupported(id) => write!(f, "plugin '{id}' is not supported on this machine"),
            Self::Plugin { id, source } => write!(f, "plugin '{id}' failed: {source}"),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Plugin { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

struct Entry {
    plugin: Box<dyn Plugin>,
    running: bool,
}

/// Owns the plugins and keeps track of which ones are running.
pub struct PluginHost {
    entries: Vec<Entry>,
}

impl PluginHost {
    pub fn new(plugins: Vec<Box<dyn Plugin>>) -> Self {
        let entries = plugins
            .into_iter()
            .map(|plugin| Entry {
                plugin,
                running: false,
            })
            .collect();
        Self { entries }
    }

    pub fn statuses(&self) -> Vec<PluginStatus> {
        self.entries
            .iter()
            .map(|entry| PluginStatus {
                info: entry.plugin.info(),
                supported: entry.plugin.is_supported(),
                running: entry.running,
            })
            .collect()
    }

    /// Starts every supported plugin the settings have enabled. One plugin failing does
    /// not keep the others from starting; the failures are returned.
    pub fn start_enabled(&mut self, settings: &Settings) -> Vec<HostError> {
        let mut failures = Vec::new();
        for entry in &mut self.entries {
            let info = entry.plugin.info();
            if entry.running || !entry.plugin.is_supported() || !settings.plugin_enabled(&info) {
                continue;
            }
            match entry.plugin.start(settings.plugin_settings(info.id)) {
                Ok(()) => entry.running = true,
                Err(source) => failures.push(HostError::Plugin {
                    id: info.id,
                    source,
                }),
            }
        }
        failures
    }

    /// Starts or stops one plugin. Asking for the state it is already in does nothing.
    pub fn set_running(&mut self, id: &str, run: bool, settings: &Value) -> Result<(), HostError> {
        let entry = self.entry_mut(id)?;
        let info = entry.plugin.info();
        if entry.running == run {
            return Ok(());
        }
        if !run {
            entry.plugin.stop();
            entry.running = false;
            return Ok(());
        }
        if !entry.plugin.is_supported() {
            return Err(HostError::Unsupported(info.id));
        }
        entry
            .plugin
            .start(settings)
            .map_err(|source| HostError::Plugin {
                id: info.id,
                source,
            })?;
        entry.running = true;
        Ok(())
    }

    /// Hands changed settings to a plugin. A plugin that is not running picks them up
    /// when it starts, so nothing is done for it here.
    pub fn apply_settings(&mut self, id: &str, settings: &Value) -> Result<(), HostError> {
        let entry = self.entry_mut(id)?;
        if !entry.running {
            return Ok(());
        }
        let info = entry.plugin.info();
        entry
            .plugin
            .apply_settings(settings)
            .map_err(|source| HostError::Plugin {
                id: info.id,
                source,
            })
    }

    /// Asks a plugin to do something. Nothing happens when it is not running.
    pub fn invoke(&mut self, id: &str, action: &str) -> Result<(), HostError> {
        let entry = self.entry_mut(id)?;
        if entry.running {
            entry.plugin.invoke(action);
        }
        Ok(())
    }

    pub fn stop_all(&mut self) {
        for entry in &mut self.entries {
            if entry.running {
                entry.plugin.stop();
                entry.running = false;
            }
        }
    }

    fn entry_mut(&mut self, id: &str) -> Result<&mut Entry, HostError> {
        self.entries
            .iter_mut()
            .find(|entry| entry.plugin.info().id == id)
            .ok_or_else(|| HostError::UnknownPlugin(id.to_owned()))
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct Calls {
        started_with: Vec<Value>,
        applied: Vec<Value>,
        invoked: Vec<String>,
        stops: usize,
    }

    struct FakePlugin {
        id: &'static str,
        enabled_by_default: bool,
        supported: bool,
        fail_start: bool,
        calls: Rc<RefCell<Calls>>,
    }

    impl FakePlugin {
        fn new(id: &'static str) -> (Self, Rc<RefCell<Calls>>) {
            let calls = Rc::new(RefCell::new(Calls::default()));
            let plugin = Self {
                id,
                enabled_by_default: true,
                supported: true,
                fail_start: false,
                calls: Rc::clone(&calls),
            };
            (plugin, calls)
        }
    }

    impl Plugin for FakePlugin {
        fn info(&self) -> PluginInfo {
            PluginInfo {
                id: self.id,
                name: "Fake",
                description: "A plugin for tests",
                enabled_by_default: self.enabled_by_default,
            }
        }

        fn is_supported(&self) -> bool {
            self.supported
        }

        fn start(&mut self, settings: &Value) -> Result<(), PluginError> {
            if self.fail_start {
                return Err("no".into());
            }
            self.calls.borrow_mut().started_with.push(settings.clone());
            Ok(())
        }

        fn stop(&mut self) {
            self.calls.borrow_mut().stops += 1;
        }

        fn apply_settings(&mut self, settings: &Value) -> Result<(), PluginError> {
            self.calls.borrow_mut().applied.push(settings.clone());
            Ok(())
        }

        fn invoke(&mut self, action: &str) {
            self.calls.borrow_mut().invoked.push(action.to_owned());
        }
    }

    #[test]
    fn start_enabled_starts_default_enabled_plugins_with_null_settings() {
        let (plugin, calls) = FakePlugin::new("a");
        let mut host = PluginHost::new(vec![Box::new(plugin)]);

        let failures = host.start_enabled(&Settings::default());

        assert!(failures.is_empty());
        assert_eq!(calls.borrow().started_with, vec![Value::Null]);
        assert!(host.statuses()[0].running);
    }

    #[test]
    fn start_enabled_skips_disabled_and_unsupported_plugins() {
        let (mut off_by_default, off_calls) = FakePlugin::new("off");
        off_by_default.enabled_by_default = false;
        let (mut unsupported, unsupported_calls) = FakePlugin::new("unsupported");
        unsupported.supported = false;
        let (switched_off, switched_off_calls) = FakePlugin::new("switched-off");
        let mut settings = Settings::default();
        settings.set_plugin_enabled("switched-off", false);
        let mut host = PluginHost::new(vec![
            Box::new(off_by_default),
            Box::new(unsupported),
            Box::new(switched_off),
        ]);

        let failures = host.start_enabled(&settings);

        assert!(failures.is_empty());
        assert!(off_calls.borrow().started_with.is_empty());
        assert!(unsupported_calls.borrow().started_with.is_empty());
        assert!(switched_off_calls.borrow().started_with.is_empty());
    }

    #[test]
    fn a_failing_plugin_does_not_stop_the_others_from_starting() {
        let (mut broken, _) = FakePlugin::new("broken");
        broken.fail_start = true;
        let (working, working_calls) = FakePlugin::new("working");
        let mut host = PluginHost::new(vec![Box::new(broken), Box::new(working)]);

        let failures = host.start_enabled(&Settings::default());

        assert_eq!(failures.len(), 1);
        assert!(matches!(
            failures[0],
            HostError::Plugin { id: "broken", .. }
        ));
        assert_eq!(working_calls.borrow().started_with.len(), 1);
        let statuses = host.statuses();
        assert!(!statuses[0].running);
        assert!(statuses[1].running);
    }

    #[test]
    fn set_running_starts_and_stops_once() {
        let (plugin, calls) = FakePlugin::new("a");
        let mut host = PluginHost::new(vec![Box::new(plugin)]);
        let settings = json!({ "size": 3 });

        host.set_running("a", true, &settings).unwrap();
        host.set_running("a", true, &settings).unwrap();
        host.set_running("a", false, &settings).unwrap();
        host.set_running("a", false, &settings).unwrap();

        assert_eq!(calls.borrow().started_with, vec![settings]);
        assert_eq!(calls.borrow().stops, 1);
    }

    #[test]
    fn set_running_rejects_unknown_and_unsupported_plugins() {
        let (mut plugin, _) = FakePlugin::new("a");
        plugin.supported = false;
        let mut host = PluginHost::new(vec![Box::new(plugin)]);

        assert!(matches!(
            host.set_running("missing", true, &Value::Null),
            Err(HostError::UnknownPlugin(_))
        ));
        assert!(matches!(
            host.set_running("a", true, &Value::Null),
            Err(HostError::Unsupported("a"))
        ));
    }

    #[test]
    fn apply_settings_only_reaches_running_plugins() {
        let (plugin, calls) = FakePlugin::new("a");
        let mut host = PluginHost::new(vec![Box::new(plugin)]);
        let settings = json!({ "size": 3 });

        host.apply_settings("a", &settings).unwrap();
        assert!(calls.borrow().applied.is_empty());

        host.set_running("a", true, &Value::Null).unwrap();
        host.apply_settings("a", &settings).unwrap();
        assert_eq!(calls.borrow().applied, vec![settings]);
    }

    #[test]
    fn invoke_only_reaches_running_plugins() {
        let (plugin, calls) = FakePlugin::new("a");
        let mut host = PluginHost::new(vec![Box::new(plugin)]);

        host.invoke("a", "wave").unwrap();
        assert!(calls.borrow().invoked.is_empty());

        host.set_running("a", true, &Value::Null).unwrap();
        host.invoke("a", "wave").unwrap();
        assert_eq!(calls.borrow().invoked, vec!["wave"]);
        assert!(matches!(
            host.invoke("missing", "wave"),
            Err(HostError::UnknownPlugin(_))
        ));
    }

    #[test]
    fn dropping_the_host_stops_running_plugins() {
        let (plugin, calls) = FakePlugin::new("a");
        let mut host = PluginHost::new(vec![Box::new(plugin)]);
        host.start_enabled(&Settings::default());

        drop(host);

        assert_eq!(calls.borrow().stops, 1);
    }
}
