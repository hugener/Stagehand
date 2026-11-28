use std::cell::{Cell, RefCell};
use std::error::Error;
use std::rc::Rc;
use std::time::Duration;

use serde_json::Value;
use slint::winit_030::WinitWindowAccessor;
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, VecModel};
use stagehand_core::{Key, PluginHost, Settings, SettingsStore, Shortcut};
use stagehand_laser_pointer::LaserPointerSettings;
use stagehand_platform::shell::{self, TaskbarWatch};
use stagehand_platform::shortcut_recorder::{self, Recorder};
use stagehand_platform::{autostart, package, paths};
use stagehand_presenter_window::PresenterWindowSettings;
use stagehand_taskbar_switch::TaskbarSwitchSettings;

use crate::{PluginEntry, SettingsWindow, ShortcutOptions, ShortcutRecording, Tray, icon, plugins};

thread_local! {
    // The application lives on the main thread. Other threads reach it by posting to
    // the event loop, which is why it is found through here instead of being passed.
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

pub struct App {
    store: SettingsStore,
    settings: RefCell<Settings>,
    host: RefCell<PluginHost>,
    // The icon in the tray. Replaced by a new one when the taskbar has lost it.
    tray: RefCell<Option<Tray>>,
    _taskbar_watch: Option<TaskbarWatch>,
    // Looks for a while after starting whether the taskbar took the icon.
    tray_check: slint::Timer,
    tray_checks_left: Cell<u32>,
    // Created when the settings are opened and dropped again when they are closed, so
    // the background process does not carry a window it is not showing.
    window: RefCell<Option<SettingsWindow>>,
    // Alive while the settings window is recording a shortcut.
    recorder: RefCell<Option<Recorder>>,
}

/// Loads the settings, starts the enabled plugins and puts the icon in the tray.
/// Must be called on the main thread, before the event loop runs.
pub fn start() -> Result<(), Box<dyn Error>> {
    let directory = paths::config_dir().ok_or("the system did not say where to keep settings")?;
    let store = SettingsStore::new(directory.join("settings.json"));
    let settings = store.load()?;

    let mut host = PluginHost::new(plugins::all());
    for failure in host.start_enabled(&settings) {
        log::error!("{failure}");
    }

    // The app may have been moved or updated since it was registered to start at
    // sign-in; registering again points the entry at where it is now. A package is
    // started by its own name, which does not change.
    if !package::is_packaged()
        && autostart::is_enabled()
        && let Err(error) = autostart::set_enabled(true)
    {
        log::error!("could not refresh the sign-in entry: {error}");
    }

    let tray = create_tray()?;

    // The taskbar drops every icon when it starts, which may be after Stagehand did:
    // at sign-in, or when Explorer is restarted.
    let taskbar_watch = TaskbarWatch::start(|| {
        // Called from inside the message loop, which is no place to change the tray.
        let _ = slint::invoke_from_event_loop(|| {
            if let Some(app) = current() {
                log::info!("the taskbar has started; putting the tray icon back");
                app.replace_tray();
            }
        });
    });
    if let Err(error) = &taskbar_watch {
        log::error!("could not watch for the taskbar starting: {error}");
    }

    let app = Rc::new(App {
        store,
        settings: RefCell::new(settings),
        host: RefCell::new(host),
        tray: RefCell::new(Some(tray)),
        _taskbar_watch: taskbar_watch.ok(),
        tray_check: slint::Timer::default(),
        tray_checks_left: Cell::new(TRAY_CHECKS),
        window: RefCell::new(None),
        recorder: RefCell::new(None),
    });
    app.tray_check
        .start(slint::TimerMode::Repeated, TRAY_CHECK_INTERVAL, || {
            if let Some(app) = current() {
                app.check_tray();
            }
        });
    APP.with_borrow_mut(|slot| *slot = Some(app));
    Ok(())
}

/// A busy taskbar, as at sign-in, can fail to take the icon without ever announcing
/// itself afterwards. So for the first minute Stagehand looks whether the icon is there.
const TRAY_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const TRAY_CHECKS: u32 = 12;

fn create_tray() -> Result<Tray, slint::PlatformError> {
    let tray = Tray::new()?;
    tray.set_app_icon(icon::tray());
    tray.on_open_settings(show_settings);
    tray.on_share_window(|| {
        invoke_plugin(
            stagehand_presenter_window::ID,
            stagehand_presenter_window::ACTION_SHARE,
        );
    });
    tray.on_stop_sharing(|| {
        invoke_plugin(
            stagehand_presenter_window::ID,
            stagehand_presenter_window::ACTION_STOP,
        );
    });
    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
    tray.show()?;
    Ok(tray)
}

/// Stops the plugins and removes the tray icon. Called once the event loop has ended.
pub fn shutdown() {
    if let Some(app) = APP.with_borrow_mut(Option::take) {
        app.host.borrow_mut().stop_all();
    }
}

/// Opens the settings window, or brings it to the front when it is already open.
/// Must be called on the main thread.
pub fn show_settings() {
    let Some(app) = current() else { return };
    if let Err(error) = app.show_settings() {
        log::error!("could not open the settings window: {error}");
    }
}

/// Asks a plugin to do something, as from the tray menu. Nothing happens when the
/// plugin is switched off.
fn invoke_plugin(id: &str, action: &str) {
    let Some(app) = current() else { return };
    if let Err(error) = app.host.borrow_mut().invoke(id, action) {
        log::error!("{error}");
    }
}

fn current() -> Option<Rc<App>> {
    APP.with_borrow(Clone::clone)
}

impl App {
    /// Takes the icon out of the tray and puts a new one in. The toolkit only asks the
    /// taskbar to show an icon when the icon is made, so this is how to ask again.
    fn replace_tray(&self) {
        // The old one has to be gone before the new one is made: they are the same
        // icon as far as the taskbar is concerned.
        self.tray.borrow_mut().take();
        match create_tray() {
            Ok(tray) => *self.tray.borrow_mut() = Some(tray),
            Err(error) => log::error!("could not put the icon in the tray: {error}"),
        }
        // The toolkit hands the icon to the taskbar when the event loop next turns,
        // which it may not do for a long time when nothing else is going on.
        slint::Timer::single_shot(Duration::ZERO, || {});
    }

    /// One of the looks, soon after starting, at whether the taskbar took the icon.
    fn check_tray(&self) {
        if !shell::tray_icon_window_exists() {
            log::warn!("the taskbar did not take the tray icon; trying again");
            self.replace_tray();
        }
        let left = self.tray_checks_left.get().saturating_sub(1);
        self.tray_checks_left.set(left);
        if left == 0 {
            self.tray_check.stop();
        }
    }

    fn show_settings(&self) -> Result<(), slint::PlatformError> {
        if self.window.borrow().is_none() {
            let window = self.create_window()?;
            *self.window.borrow_mut() = Some(window);
        }
        let window = self.window.borrow();
        let window = window.as_ref().expect("the window was created above");
        window.show()?;
        window.window().set_minimized(false);
        let reached = window.window().with_winit_window(|native| {
            icon::set_on_window(native);
            native.focus_window();
        });
        if reached.is_none() {
            // The first time, the native window only comes into being when the event
            // loop next turns.
            let _ = slint::invoke_from_event_loop(|| {
                let Some(app) = current() else { return };
                if let Some(window) = app.window.borrow().as_ref() {
                    window.window().with_winit_window(|native| {
                        icon::set_on_window(native);
                        native.focus_window();
                    });
                }
            });
        }
        Ok(())
    }

    fn create_window(&self) -> Result<SettingsWindow, slint::PlatformError> {
        let window = SettingsWindow::new()?;
        window.set_version(env!("CARGO_PKG_VERSION").into());
        window.set_start_at_login(autostart::is_enabled());
        window.set_plugins(self.plugin_entries());

        window.on_start_at_login_toggled(|enabled| {
            let Some(app) = current() else { return };
            if let Err(error) = autostart::set_enabled(enabled) {
                log::error!("could not change the sign-in entry: {error}");
            }
            // Show what the system now says, which is the old state if the change failed.
            if let Some(window) = app.window.borrow().as_ref() {
                window.set_start_at_login(autostart::is_enabled());
            }
        });

        window.on_plugin_toggled(|id, enabled| {
            let Some(app) = current() else { return };
            app.set_plugin_enabled(&id, enabled);
        });

        let taskbar_switch = TaskbarSwitchSettings::from_value(
            self.settings
                .borrow()
                .plugin_settings(stagehand_taskbar_switch::ID),
        );
        window.set_taskbar_switch(plugins::taskbar_switch_options(&taskbar_switch));
        window.on_taskbar_switch_changed(|options| {
            let Some(app) = current() else { return };
            let settings = plugins::taskbar_switch_settings(&options);
            app.set_plugin_settings(stagehand_taskbar_switch::ID, settings.to_value());
        });

        let laser_pointer = LaserPointerSettings::from_value(
            self.settings
                .borrow()
                .plugin_settings(stagehand_laser_pointer::ID),
        );
        window.set_shortcut_keys(plugins::shortcut_keys());
        window.set_laser_pointer(plugins::laser_pointer_options(&laser_pointer));
        window.on_laser_pointer_changed(|options| {
            let Some(app) = current() else { return };
            app.save_laser_pointer(&options);
        });

        let presenter_window = PresenterWindowSettings::from_value(
            self.settings
                .borrow()
                .plugin_settings(stagehand_presenter_window::ID),
        );
        window.set_presenter_window(plugins::presenter_window_options(&presenter_window));
        window.on_presenter_window_changed(|options| {
            let Some(app) = current() else { return };
            app.save_presenter_window(&options);
        });

        let recording = window.global::<ShortcutRecording>();
        recording.on_start(|target| {
            let Some(app) = current() else { return };
            app.start_recording(&target);
        });
        recording.on_stop(|| {
            let Some(app) = current() else { return };
            app.stop_recording();
        });

        window.window().on_close_requested(|| {
            // The window cannot be dropped from inside its own callback, so that is
            // left to the event loop's next turn.
            let _ = slint::invoke_from_event_loop(|| {
                if let Some(app) = current() {
                    app.recorder.borrow_mut().take();
                    app.window.borrow_mut().take();
                }
            });
            CloseRequestResponse::HideWindow
        });

        Ok(window)
    }

    fn save_laser_pointer(&self, options: &crate::LaserPointerOptions) {
        let settings = plugins::laser_pointer_settings(options);
        self.set_plugin_settings(stagehand_laser_pointer::ID, settings.to_value());
    }

    fn save_presenter_window(&self, options: &crate::PresenterWindowOptions) {
        let settings = plugins::presenter_window_settings(options);
        self.set_plugin_settings(stagehand_presenter_window::ID, settings.to_value());
    }

    /// Starts listening for the keys of the shortcut the settings window calls `target`.
    fn start_recording(&self, target: &str) {
        let window = self.window.borrow();
        let Some(window) = window.as_ref() else {
            return;
        };
        // Only one shortcut is recorded at a time.
        self.recorder.borrow_mut().take();
        let recorder = Recorder::start(|event| {
            // Called in the middle of the system handing over a key, which is no place
            // to change the window from.
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = current() {
                    app.recorded(event);
                }
            });
        });
        let recording = window.global::<ShortcutRecording>();
        match recorder {
            Ok(recorder) => {
                *self.recorder.borrow_mut() = Some(recorder);
                recording.set_held(ShortcutOptions::default());
                recording.set_hint("".into());
                recording.set_target(target.into());
            }
            Err(error) => {
                log::error!("could not record a shortcut: {error}");
                recording.set_target("".into());
            }
        }
    }

    fn stop_recording(&self) {
        self.recorder.borrow_mut().take();
        if let Some(window) = self.window.borrow().as_ref() {
            window.global::<ShortcutRecording>().set_target("".into());
        }
    }

    /// Handles a key pressed while a shortcut is being recorded.
    fn recorded(&self, event: shortcut_recorder::Event) {
        // A key that was already on its way when recording stopped.
        if self.recorder.borrow().is_none() {
            return;
        }
        let (modifiers, virtual_key) = match event {
            shortcut_recorder::Event::Held(modifiers) => (modifiers, None),
            shortcut_recorder::Event::Pressed {
                modifiers,
                virtual_key,
            } => (modifiers, Some(virtual_key)),
        };
        if virtual_key == Some(shortcut_recorder::ESCAPE) && !modifiers.any() {
            self.stop_recording();
            return;
        }

        let hint = {
            let window = self.window.borrow();
            let Some(window) = window.as_ref() else {
                return;
            };
            let recording = window.global::<ShortcutRecording>();
            recording.set_held(ShortcutOptions {
                win: modifiers.win,
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                key: 0,
            });
            let Some(virtual_key) = virtual_key else {
                return;
            };
            let target = recording.get_target();
            let key = Key::all().find(|key| key.virtual_key() == virtual_key);
            let shortcut = key.map(|key| Shortcut {
                win: modifiers.win,
                ctrl: modifiers.ctrl,
                alt: modifiers.alt,
                shift: modifiers.shift,
                key,
            });
            match shortcut {
                None => Some("Use a letter, a digit or F1 to F24 as the key.".to_owned()),
                Some(shortcut) if !shortcut.is_usable() => {
                    Some("Hold Win, Ctrl or Alt together with a letter or a digit.".to_owned())
                }
                Some(shortcut) => match plugins::shortcut_user(window, shortcut, &target) {
                    Some(user) => Some(format!("{shortcut} is already used to {user}.")),
                    None => {
                        self.set_shortcut(window, &target, shortcut);
                        None
                    }
                },
            }
        };
        match hint {
            Some(hint) => {
                if let Some(window) = self.window.borrow().as_ref() {
                    window.global::<ShortcutRecording>().set_hint(hint.into());
                }
            }
            None => self.stop_recording(),
        }
    }

    /// Makes `shortcut` the one the settings window calls `target`, and saves it.
    fn set_shortcut(&self, window: &SettingsWindow, target: &str, shortcut: Shortcut) {
        let shortcut = plugins::shortcut_options(Some(shortcut));
        let mut laser_pointer = window.get_laser_pointer();
        let mut presenter_window = window.get_presenter_window();
        match target {
            plugins::LASER_MOUSE_SHORTCUT => laser_pointer.mouse_shortcut = shortcut,
            plugins::LASER_PEN_SHORTCUT => laser_pointer.pen_shortcut = shortcut,
            plugins::PRESENTER_SHARE_SHORTCUT => presenter_window.share_shortcut = shortcut,
            plugins::PRESENTER_STOP_SHORTCUT => presenter_window.stop_shortcut = shortcut,
            _ => {
                log::error!("the settings window recorded an unknown shortcut: {target}");
                return;
            }
        }
        if target.starts_with(stagehand_laser_pointer::ID) {
            self.save_laser_pointer(&laser_pointer);
            window.set_laser_pointer(laser_pointer);
        } else {
            self.save_presenter_window(&presenter_window);
            window.set_presenter_window(presenter_window);
        }
    }

    fn set_plugin_enabled(&self, id: &str, enabled: bool) {
        let mut settings = self.settings.borrow_mut();
        let result = self
            .host
            .borrow_mut()
            .set_running(id, enabled, settings.plugin_settings(id));
        match result {
            Ok(()) => {
                settings.set_plugin_enabled(id, enabled);
                if let Err(error) = self.store.save(&settings) {
                    log::error!("could not save the settings: {error}");
                }
            }
            Err(error) => log::error!("{error}"),
        }
        drop(settings);
        // Redraw the list from what is actually running, so a plugin that failed to
        // start does not keep its switch on.
        if let Some(window) = self.window.borrow().as_ref() {
            window.set_plugins(self.plugin_entries());
        }
    }

    /// Stores a plugin's settings and hands them to the plugin if it is running.
    fn set_plugin_settings(&self, id: &str, value: Value) {
        let mut settings = self.settings.borrow_mut();
        if let Err(error) = self.host.borrow_mut().apply_settings(id, &value) {
            log::error!("{error}");
        }
        // Stored even when the plugin refused them, so that what the window shows and
        // what is saved stay the same.
        settings.set_plugin_settings(id, value);
        if let Err(error) = self.store.save(&settings) {
            log::error!("could not save the settings: {error}");
        }
    }

    fn plugin_entries(&self) -> ModelRc<PluginEntry> {
        let entries: Vec<PluginEntry> = self
            .host
            .borrow()
            .statuses()
            .into_iter()
            .map(|status| PluginEntry {
                id: status.info.id.into(),
                name: status.info.name.into(),
                description: status.info.description.into(),
                enabled: status.running,
                supported: status.supported,
            })
            .collect();
        ModelRc::new(VecModel::from(entries))
    }
}
