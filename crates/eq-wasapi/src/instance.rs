//! Single-instance guard and "show your window" requests between app instances.
//!
//! The first instance owns a named mutex and an auto-reset event. A second instance finds the
//! mutex already present, signals the event and exits. The first instance waits on the event
//! and reports each request through a callback. Names are `Local\`-scoped, so they never leave
//! the user's session.

use std::io;
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::{HSTRING, PCWSTR};

use crate::error::{ComResultExt, WasapiError};

/// Name of the mutex object for an application called `name`: `Local\{name}`.
#[must_use]
fn mutex_name(name: &str) -> String {
    format!(r"Local\{name}")
}

/// Name of the auto-reset event that asks the first instance to show its window.
#[must_use]
fn show_event_name(name: &str) -> String {
    format!(r"Local\{name}-show")
}

/// Owns a kernel handle and closes it exactly once, on drop.
struct OwnedHandle(HANDLE);

// SAFETY: a kernel object handle is not bound to a thread. Waiting on it, signalling it and
// closing it are allowed from any thread, and the wrapper never hands out mutable access.
unsafe impl Send for OwnedHandle {}
// SAFETY: see the `Send` impl above; shared use only calls thread-agnostic kernel functions.
unsafe impl Sync for OwnedHandle {}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by a successful `Create*`/`Open*` call and is closed
        // only here. A failing close leaves nothing to recover, so the result is ignored.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The events shared between the instance and its listener thread.
///
/// The listener holds an `Arc` clone, so the handles stay open while it can still wait on them.
struct Events {
    /// Set by a second instance to ask for the window.
    show: OwnedHandle,
    /// Set when the owning instance is dropped, to end the listener.
    stop: OwnedHandle,
}

/// Holds the single-instance mutex of the application for as long as it lives.
///
/// Dropping the value releases the mutex (so a later launch becomes the first instance) and ends
/// the listener thread started by [`SingleInstance::wait_show_requests`].
pub struct SingleInstance {
    /// Existence of this mutex marks the application as running.
    _mutex: OwnedHandle,
    events: Arc<Events>,
}

impl SingleInstance {
    /// Claims the single-instance slot called `name`.
    ///
    /// Returns `Ok(Some(_))` when this process is the first instance, and `Ok(None)` when another
    /// instance already holds the mutex.
    ///
    /// # Errors
    ///
    /// Returns [`WasapiError::Com`] when the mutex or the events cannot be created.
    pub fn acquire(name: &str) -> Result<Option<Self>, WasapiError> {
        let mutex_wide = HSTRING::from(mutex_name(name));
        // SAFETY: default security attributes, no initial owner, and `mutex_wide` outlives the
        // call. The handle is wrapped at once, so every return path below closes it.
        let mutex =
            OwnedHandle(unsafe { CreateMutexW(None, false, &mutex_wide) }.context("CreateMutexW")?);
        // SAFETY: plain read of the thread's last-error value, set by the `CreateMutexW` call
        // above on this same thread. Nothing ran in between that could overwrite it.
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            return Ok(None);
        }

        let show_wide = HSTRING::from(show_event_name(name));
        // SAFETY: default security attributes, auto-reset and non-signalled; `show_wide`
        // outlives the call. The handle is wrapped at once.
        let show = OwnedHandle(
            unsafe { CreateEventW(None, false, false, &show_wide) }
                .context("CreateEventW(show)")?,
        );
        // SAFETY: unnamed manual-reset event, non-signalled. A null name is accepted by the API.
        let stop = OwnedHandle(
            unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
                .context("CreateEventW(stop)")?,
        );
        Ok(Some(Self {
            _mutex: mutex,
            events: Arc::new(Events { show, stop }),
        }))
    }

    /// Asks the running first instance called `name` to show its window.
    ///
    /// # Errors
    ///
    /// Returns [`WasapiError::Com`] when the show event does not exist (the first instance is
    /// gone or still starting) or cannot be signalled.
    pub fn signal_existing(name: &str) -> Result<(), WasapiError> {
        let show_wide = HSTRING::from(show_event_name(name));
        // SAFETY: `show_wide` outlives the call; the access right is the documented constant.
        let event = OwnedHandle(
            unsafe { OpenEventW(EVENT_MODIFY_STATE, false, &show_wide) }
                .context("OpenEventW(show)")?,
        );
        // SAFETY: `event` is a valid handle opened with `EVENT_MODIFY_STATE`, which is the access
        // that `SetEvent` needs. The handle stays open until `event` is dropped at return.
        unsafe { SetEvent(event.0) }.context("SetEvent(show)")
    }

    /// Starts a thread that calls `on_show` each time a second instance asks for the window.
    ///
    /// The thread ends when this value is dropped. Requests that arrive while `on_show` runs are
    /// kept by the auto-reset event and handled afterwards.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the thread cannot be created.
    pub fn wait_show_requests(
        &self,
        mut on_show: impl FnMut() + Send + 'static,
    ) -> io::Result<JoinHandle<()>> {
        let events = Arc::clone(&self.events);
        thread::Builder::new()
            .name("show-requests".to_owned())
            .spawn(move || {
                loop {
                    let handles = [events.show.0, events.stop.0];
                    // SAFETY: both handles belong to `events`, which this closure keeps alive
                    // through its `Arc`. The wait has no timeout and returns when either event
                    // is signalled.
                    let wait = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                    if wait == WAIT_OBJECT_0 {
                        on_show();
                    } else {
                        // The stop event (index 1) or a failed wait both end the listener.
                        break;
                    }
                }
            })
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: the stop handle is kept open by `self.events`, which this value owns. A failed
        // signal only delays the listener's exit, so the result is ignored.
        let _ = unsafe { SetEvent(self.events.stop.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_local_and_suffixed() {
        assert_eq!(mutex_name("equalizerer"), r"Local\equalizerer");
        assert_eq!(show_event_name("equalizerer"), r"Local\equalizerer-show");
    }

    #[test]
    fn second_acquire_of_the_same_name_is_refused() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let name = format!("eq-test-instance-{}-{nanos}", std::process::id());

        let first = SingleInstance::acquire(&name).expect("first acquire");
        assert!(first.is_some(), "the first acquire must succeed");
        let second = SingleInstance::acquire(&name).expect("second acquire");
        assert!(second.is_none(), "the second acquire must be refused");
    }
}
