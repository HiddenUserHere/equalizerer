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
