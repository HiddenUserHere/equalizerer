//! Panic hook: logs the panic and gives the user their real output device back.
//!
//! While the app runs, the virtual device is the system default. A panic ends the process without
//! the controller's shutdown path, so the hook restores the persisted target itself. The hook
//! must never panic, so every failure is logged and ignored.

use std::panic;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use eq_core::SettingsStore;

use crate::backend;

/// Set while a restore runs, so a panic inside the restore cannot start another restore.
static RESTORING: AtomicBool = AtomicBool::new(false);

/// Installs the hook. It chains to the previous hook, so the default output still goes to stderr.
///
/// `settings_file` is read when a panic happens to find the target device.
pub fn install(settings_file: PathBuf) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("application panicked: {info}\n{backtrace}");
        previous(info);
        restore_from_settings(&settings_file);
    }));
}

/// Restores the target device recorded in the settings file, if there is one.
fn restore_from_settings(settings_file: &Path) {
    if RESTORING.swap(true, Ordering::SeqCst) {
        return;
    }
    // A corrupt settings file is quarantined by `load`; the restore then has no target and stops.
    let target = SettingsStore::new(settings_file.to_path_buf())
        .load()
        .target_device_id;
    if let Some(target) = target {
        // A fresh thread gives the restore a clean COM apartment, whatever state the panicking
        // thread left behind.
        match thread::Builder::new()
            .name("panic-restore".to_owned())
            .spawn(move || backend::restore_default(&target))
        {
            Ok(handle) => match handle.join() {
                Ok(Ok(())) => tracing::error!("default output restored after the panic"),
                Ok(Err(error)) => {
                    tracing::error!(
                        "could not restore the default output after the panic: {error}"
                    );
                }
                Err(_) => tracing::error!("the restore thread panicked"),
            },
            Err(error) => tracing::error!("cannot start the restore thread: {error}"),
        }
    }
    RESTORING.store(false, Ordering::SeqCst);
}
