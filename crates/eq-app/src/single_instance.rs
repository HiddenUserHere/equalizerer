//! Single-instance handling: the first launch runs the app, later launches ask it to show its
//! window and exit.
//!
//! Only Windows has the mechanism so far (a named mutex and event, see `eq-wasapi`). Elsewhere
//! every launch is the first instance until the platform backend adds its own guard.

use crossbeam_channel::Sender;

/// Name the single-instance objects are built from.
const APP_NAME: &str = "equalizerer";

/// Outcome of [`claim`].
pub enum Claim {
    /// This process is the first instance. Keep the [`Held`] value alive until the app exits.
    Primary(Held),
    /// Another instance is running and has been asked to show its window. Exit now.
    AlreadyRunning,
}

/// Proof that this process is the first instance. Dropping it lets the next launch take over.
#[cfg(windows)]
pub struct Held {
    _instance: eq_wasapi::SingleInstance,
}

/// Proof that this process is the first instance. Dropping it lets the next launch take over.
#[cfg(not(windows))]
pub struct Held;

/// Claims the single-instance slot. When another instance already holds it, asks that instance
/// to show its window.
///
/// Each show request sends `()` on `show`; the caller turns it into a window raise. The
/// listener runs until the returned [`Held`] is dropped.
///
/// # Errors
///
/// Returns a message for the log when the slot cannot be checked, the listener cannot start, or
/// the running instance cannot be signalled.
#[cfg(windows)]
pub fn claim(show: Sender<()>) -> Result<Claim, String> {
    let instance = eq_wasapi::SingleInstance::acquire(APP_NAME)
        .map_err(|error| format!("single-instance check failed: {error}"))?;
    if let Some(instance) = instance {
        // The listener's handle is detached on purpose: it ends when `instance` is dropped.
        instance
            .wait_show_requests(move || {
                // A closed channel means the UI is gone; there is no window to raise then.
                let _ = show.send(());
            })
            .map_err(|error| format!("cannot start the show-request listener: {error}"))?;
        Ok(Claim::Primary(Held {
            _instance: instance,
        }))
    } else {
        eq_wasapi::SingleInstance::signal_existing(APP_NAME).map_err(|error| {
            format!("another instance runs but could not be asked to show: {error}")
        })?;
        Ok(Claim::AlreadyRunning)
    }
}

/// Claims the single-instance slot. No mechanism exists on this platform yet, so this process is
/// always the first instance.
///
/// # Errors
///
/// Never fails on this platform; the signature matches the Windows version.
#[cfg(not(windows))]
pub fn claim(_show: Sender<()>) -> Result<Claim, String> {
    Ok(Claim::Primary(Held))
}
