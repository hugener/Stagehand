// Release builds are a background app with no console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod icon;
mod logging;
mod plugins;

use std::error::Error;
use std::process::ExitCode;

use slint::winit_030::winit::event_loop::{ActiveEventLoop, DeviceEvents};
use slint::winit_030::{CustomApplicationHandler, EventResult};
use stagehand_platform::single_instance::{self, Instance};
use stagehand_platform::{autostart, package};

slint::include_modules!();

/// Keeps the window toolkit from asking Windows for raw keyboard and mouse input, which
/// it does by default and Stagehand has no use for. While a program receives raw
/// keyboard input and its window is in front, Windows no longer calls that program's
/// own keyboard hook, which is what the settings window records shortcuts with.
struct NoRawInput;

impl CustomApplicationHandler for NoRawInput {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) -> EventResult {
        event_loop.listen_device_events(DeviceEvents::Never);
        EventResult::Propagate
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            log::error!("Stagehand could not start: {error}");
            eprintln!("stagehand: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    // A background start is the system launching Stagehand at sign-in. Any other start
    // is the user asking for it, which opens the settings.
    let background = std::env::args()
        .skip(1)
        .any(|argument| argument == autostart::BACKGROUND_ARG)
        || package::started_at_sign_in();

    let instance = match single_instance::acquire("Stagehand")? {
        Instance::Primary(instance) => instance,
        Instance::Secondary(running) => {
            if !background {
                running.activate_primary()?;
            }
            return Ok(());
        }
    };

    // Stagehand is still useful without a log, so failing to start one is not fatal.
    let _ = logging::start();
    log::info!("Stagehand {} started", env!("CARGO_PKG_VERSION"));

    slint::BackendSelector::new()
        .with_winit_custom_application_handler(NoRawInput)
        .select()?;
    app::start()?;
    instance.on_activate(|| {
        let _ = slint::invoke_from_event_loop(app::show_settings);
    })?;
    if !background {
        app::show_settings();
    }

    slint::run_event_loop_until_quit()?;
    app::shutdown();
    log::info!("Stagehand stopped");
    Ok(())
}
