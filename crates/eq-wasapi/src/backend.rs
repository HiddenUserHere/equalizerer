//! The Windows audio backend: one value that bundles every adapter the controller needs.

use crossbeam_channel::Sender;
use eq_core::{
    AudioBackend, DeviceEvent, EndpointDirectory, PatternLocator, PipelineFactory,
    VirtualDeviceProvider,
};

use crate::com::ComApartment;
use crate::devices::MmDirectory;
use crate::error::WasapiError;
use crate::notifications::NotificationGuard;
use crate::pipeline::WasapiPipelineFactory;
use crate::virtual_device::CableProvider;

/// Windows implementation of [`AudioBackend`].
///
/// Create it and use it on one thread: the controller thread, inside the factory closure given
/// to `Controller::spawn`. COM objects are bound to the thread that created them, so the backend
/// is neither `Send` nor `Sync`.
///
/// The fields are declared in drop order. Rust drops them top to bottom, so notifications are
/// unregistered first, then the enumerator is released, and the COM apartment is uninitialised
/// last.
pub struct WasapiBackend {
    /// Keeps the device notification client registered. Dropped first.
    _notifications: NotificationGuard,
    /// Device listing and default-device control.
    directory: MmDirectory,
    /// Locates the VB-CABLE endpoint by name.
    provider: CableProvider,
    /// Builds the real-time pipelines.
    factory: WasapiPipelineFactory,
    /// COM apartment of this thread. Dropped last, after every COM object above.
    _com: ComApartment,
}

impl WasapiBackend {
    /// Initialises COM on the current thread and builds every adapter.
    ///
    /// `device_events` receives device notifications and pipeline failures. `virtual_pattern`
    /// is the case-insensitive name fragment of the virtual playback device (for example,
    /// `CABLE Input`).
    ///
    /// # Errors
    ///
    /// Returns [`WasapiError`] when COM cannot be initialised, the device enumerator cannot be
    /// created, or Windows rejects the notification registration.
    pub fn new(
        device_events: Sender<DeviceEvent>,
        virtual_pattern: &str,
    ) -> Result<Self, WasapiError> {
        let com = ComApartment::init_mta()?;
        let locator = PatternLocator::new(virtual_pattern);
        let directory = MmDirectory::new(&com, locator.clone())?;
        let notifications =
            NotificationGuard::register(directory.enumerator(), device_events.clone())?;
        let factory = WasapiPipelineFactory::new(device_events);
        let provider = CableProvider::new(locator);
        Ok(Self {
            _notifications: notifications,
            directory,
            provider,
            factory,
            _com: com,
        })
    }
}

impl AudioBackend for WasapiBackend {
    fn directory(&self) -> &dyn EndpointDirectory {
        &self.directory
    }

    fn virtual_provider(&mut self) -> &mut dyn VirtualDeviceProvider {
        &mut self.provider
    }

    fn pipeline_factory(&self) -> &dyn PipelineFactory {
        &self.factory
    }
}
