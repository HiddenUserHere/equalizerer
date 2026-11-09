//! Equalizerer application entry point (composition root).
//!
//! Reads the mode from the arguments, sets up logging and the single-instance check, connects the
//! controller to the platform backend and the Slint window, and waits until everything has
//! stopped. The platform is chosen only in `backend`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod args;
mod backend;
mod demo;
mod icon;
mod logging;
mod panic;
mod single_instance;
mod tray;
mod ui;

use std::process::ExitCode;

use crossbeam_channel::{Receiver, unbounded};
use eq_core::{
    AppCommand, AppEvent, AppPaths, Controller, ControllerConfig, DeviceEvent, PresetStore,
    SettingsStore,
};

use args::{Mode, Options};
use single_instance::Claim;
use ui::UiOptions;

fn main() -> ExitCode {
    let options = match args::parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let Some(paths) = AppPaths::discover() else {
        // Without a home directory there is nowhere to log to, so stderr is the only channel.
        eprintln!("cannot find the user's configuration directory; nothing was started");
        return ExitCode::from(1);
    };
    // Kept alive until `main` returns, so the log is flushed on every exit path below.
    let _log_guard = logging::init(&paths.log_dir);
    tracing::info!(?options, version = env!("CARGO_PKG_VERSION"), "starting");

    match options.mode {
        Mode::Restore => restore(&paths),
        Mode::App | Mode::Demo => run_session(options, &paths),
    }
}

/// `--restore`: makes the persisted target the default output again, then exits.
fn restore(paths: &AppPaths) -> ExitCode {
    let settings = SettingsStore::new(paths.settings_file.clone()).load();
    let Some(target) = settings.target_device_id else {
        tracing::info!("no target device is recorded; nothing to restore");
        return ExitCode::SUCCESS;
    };
    match backend::restore_default(&target) {
        Ok(()) => {
            tracing::info!(target = target.as_str(), "default output restored");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!("restoring the default output failed: {error}");
            ExitCode::from(1)
        }
    }
}

/// Runs the window, unless another instance already runs. Then the window is the first
/// instance's and this process only asks it to show itself.
fn run_session(options: Options, paths: &AppPaths) -> ExitCode {
    let (show_tx, show_rx) = unbounded::<()>();
    // Held for the whole session; dropping it lets the next launch become the first instance.
    let _held = match single_instance::claim(show_tx) {
        Ok(Claim::Primary(held)) => held,
        Ok(Claim::AlreadyRunning) => {
            tracing::info!("another instance is running; asked it to show its window");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            tracing::error!("{message}");
            return ExitCode::from(1);
        }
    };
    match options.mode {
        Mode::Demo => run_demo(show_rx, options.start_minimized),
        Mode::App | Mode::Restore => run_app(paths, show_rx, options.start_minimized),
    }
}

/// `--demo`: the window with the stand-in controller, for UI work without audio.
fn run_demo(show_requests: Receiver<()>, start_minimized: bool) -> ExitCode {
    tracing::info!("running the demo controller (--demo)");
    let (command_tx, command_rx) = unbounded::<AppCommand>();
    let (event_tx, event_rx) = unbounded::<AppEvent>();
    let controller = demo::spawn(command_rx, event_tx);
    // The demo never writes the "Start with Windows" setting: it only shows the checkbox.
    let ui_options = UiOptions {
        start_minimized,
        persist_autostart: false,
    };
    let result = ui::run_ui(command_tx, event_rx, show_requests, ui_options);
    // The demo stops when its command channel closes, which the failed or closed UI caused.
    if controller.join().is_err() {
        tracing::error!("the demo controller panicked");
    }
    finish_ui(result)
}

/// Normal mode: the real controller with the platform backend, and the window.
fn run_app(paths: &AppPaths, show_requests: Receiver<()>, start_minimized: bool) -> ExitCode {
    let settings_store = SettingsStore::new(paths.settings_file.clone());
    let settings = settings_store.load();
    let presets = match PresetStore::open(paths.presets_dir.clone()) {
        Ok(presets) => presets,
        Err(error) => {
            tracing::error!(
                "cannot use the presets directory {}: {error}",
                paths.presets_dir.display()
            );
            return ExitCode::from(1);
        }
    };
    for problem in presets.load_errors() {
        tracing::warn!("skipped a preset file: {problem}");
    }
    panic::install(paths.settings_file.clone());

    let (command_tx, command_rx) = unbounded::<AppCommand>();
    let (event_tx, event_rx) = unbounded::<AppEvent>();
    let (device_tx, device_rx) = unbounded::<DeviceEvent>();
    let pattern = settings.virtual_device_pattern.clone();
    let controller = match Controller::spawn(
        move || backend::build_backend(device_tx, &pattern),
        presets,
        settings_store,
        settings,
        ControllerConfig::default(),
        event_tx,
        command_rx,
        device_rx,
    ) {
        Ok(handle) => handle,
        Err(error) => {
            tracing::error!("cannot start the controller thread: {error}");
            return ExitCode::from(1);
        }
    };

    // The window sends Quit when it closes. If it fails before that, this copy of the sender
    // still ends the controller; without a Quit the join below would wait forever.
    let quit_tx = command_tx.clone();
    let ui_options = UiOptions {
        start_minimized,
        persist_autostart: true,
    };
    let result = ui::run_ui(command_tx, event_rx, show_requests, ui_options);
    let _ = quit_tx.send(AppCommand::Quit);
    if controller.join().is_err() {
        tracing::error!("the controller thread panicked");
    }
    finish_ui(result)
}

/// Maps the window's result to the exit code.
fn finish_ui(result: Result<(), slint::PlatformError>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("the window failed: {error}");
            ExitCode::from(1)
        }
    }
}
