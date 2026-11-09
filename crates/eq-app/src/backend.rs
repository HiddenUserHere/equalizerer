//! Selects the platform audio backend. This is the only module that names a backend crate.
//!
//! The rest of the application talks to the `eq-core` ports; the platform is chosen here, at
//! compile time.

use crossbeam_channel::Sender;
use eq_core::{AudioBackend, BackendError, DeviceEvent, DeviceId};

/// Builds the platform backend. Runs on the controller thread, because the backend's OS objects
/// (COM apartment, `PipeWire` loop) must be created and used on one thread.
///
/// # Errors
///
/// Returns [`BackendError`] when the platform backend cannot start, or when the platform has no
/// backend yet.
#[cfg(windows)]
pub fn build_backend(
    device_events: Sender<DeviceEvent>,
    virtual_pattern: &str,
) -> Result<Box<dyn AudioBackend>, BackendError> {
    let backend = eq_wasapi::WasapiBackend::new(device_events, virtual_pattern)?;
    Ok(Box::new(backend))
}

/// Builds the platform backend. Linux has no backend yet; the PipeWire backend is planned.
///
/// # Errors
///
/// Always returns [`BackendError::Unavailable`] until the PipeWire backend exists.
#[cfg(not(windows))]
pub fn build_backend(
    _device_events: Sender<DeviceEvent>,
    _virtual_pattern: &str,
) -> Result<Box<dyn AudioBackend>, BackendError> {
    Err(BackendError::Unavailable(
        "this platform has no audio backend yet".to_owned(),
    ))
}

/// Makes `target` the default output again, without a running controller.
///
/// Used by `--restore` and by the panic hook.
///
/// # Errors
///
/// Returns the failure as text, because the caller only logs it.
#[cfg(windows)]
pub fn restore_default(target: &DeviceId) -> Result<(), String> {
    eq_wasapi::restore_default_device(target).map_err(|error| error.to_string())
}

/// Makes `target` the default output again. No backend exists on this platform yet.
///
/// # Errors
///
/// Always returns an error until the PipeWire backend exists.
#[cfg(not(windows))]
pub fn restore_default(_target: &DeviceId) -> Result<(), String> {
    Err("restoring the default output is not supported on this platform yet".to_owned())
}

/// Name of the value in the per-user `Run` key. Windows shows it in the startup settings.
#[cfg(windows)]
const AUTOSTART_VALUE: &str = "Equalizerer";

/// Whether "Start with Windows" can be offered on this platform.
#[cfg(windows)]
#[must_use]
pub fn autostart_available() -> bool {
    true
}

/// Whether "Start with Windows" can be offered on this platform. It cannot yet elsewhere.
#[cfg(not(windows))]
#[must_use]
pub fn autostart_available() -> bool {
    false
}

/// Tells whether the app starts with Windows.
///
/// # Errors
///
/// Returns the failure as text, because the caller only shows it.
#[cfg(windows)]
pub fn autostart_is_enabled() -> Result<bool, String> {
    eq_wasapi::is_autostart_enabled(AUTOSTART_VALUE).map_err(|error| error.to_string())
}

/// Tells whether the app starts with Windows. Always `false` where the setting does not exist.
///
/// # Errors
///
/// Never fails on this platform; the signature matches the Windows version.
#[cfg(not(windows))]
pub fn autostart_is_enabled() -> Result<bool, String> {
    Ok(false)
}

/// Starts the app with Windows (`enabled`) or stops that. Starting runs this executable with
/// `--minimized`, so the first start shows only the tray icon.
///
/// # Errors
///
/// Returns the failure as text, because the caller only shows it.
#[cfg(windows)]
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    if enabled {
        let program = std::env::current_exe()
            .map_err(|error| format!("cannot find the program file: {error}"))?;
        // The path is quoted because it may contain spaces; the shell splits the command line.
        let command = format!("\"{}\" --minimized", program.display());
        eq_wasapi::enable_autostart(AUTOSTART_VALUE, &command).map_err(|error| error.to_string())
    } else {
        eq_wasapi::disable_autostart(AUTOSTART_VALUE).map_err(|error| error.to_string())
    }
}

/// Starts the app with the OS or stops that. No setting exists on this platform yet.
///
/// # Errors
///
/// Always returns an error until the platform has the setting.
#[cfg(not(windows))]
pub fn set_autostart(_enabled: bool) -> Result<(), String> {
    Err("starting with the system is not supported on this platform yet".to_owned())
}
