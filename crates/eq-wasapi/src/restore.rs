//! Restores a default output device from outside the controller, for `--restore` and the panic
//! hook.

use eq_core::DeviceId;

use crate::com::ComApartment;
use crate::error::WasapiError;
use crate::policy_config::set_default_endpoint;

/// Makes `target` the default output for the console and multimedia roles.
///
/// Initializes COM on the calling thread for the duration of the call, so it works on any thread
/// (including a fresh thread started by a panic hook). It does not need the controller, the
/// device notifications or the device directory.
///
/// # Errors
///
/// Returns [`WasapiError::Com`] when COM cannot be initialized or the policy client rejects the
/// change, and [`WasapiError::DeviceNotFound`] when Windows does not know `target`.
pub fn restore_default_device(target: &DeviceId) -> Result<(), WasapiError> {
    let _com = ComApartment::init_mta()?;
    set_default_endpoint(target)
}
