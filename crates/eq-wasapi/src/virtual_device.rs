//! Virtual device provider for Windows: locates a pre-installed VB-CABLE endpoint by name.

use eq_core::{DeviceId, DeviceInfo, PatternLocator, VirtualDeviceProvider, VirtualError};

/// Provides the VB-CABLE playback endpoint that system audio is routed into.
///
/// The app does not create this device: the user installs VB-CABLE once. `ensure` looks it up in
/// the device list passed by the controller, and `release` does nothing because the endpoint
/// belongs to the driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CableProvider {
    locator: PatternLocator,
}

impl CableProvider {
    /// Creates a provider that finds the endpoint matching `locator`.
    #[must_use]
    pub fn new(locator: PatternLocator) -> Self {
        Self { locator }
    }
}

impl Default for CableProvider {
    /// Finds `CABLE Input`, the playback endpoint of VB-CABLE.
    fn default() -> Self {
        Self::new(PatternLocator::default())
    }
}

impl VirtualDeviceProvider for CableProvider {
    /// Returns the id of the first device whose name matches the locator.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualError::NotFound`] when no device matches, which usually means VB-CABLE
    /// is not installed or is disabled.
    fn ensure(&mut self, devices: &[DeviceInfo]) -> Result<DeviceId, VirtualError> {
        self.locator
            .locate(devices)
            .map(|device| device.id.clone())
            .ok_or_else(|| VirtualError::NotFound(self.locator.pattern().to_owned()))
    }

    /// Does nothing: the endpoint is owned by the VB-CABLE driver, not by this process.
    fn release(&mut self) -> Result<(), VirtualError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: name.to_owned(),
            is_virtual: false,
        }
    }

    #[test]
    fn ensure_returns_the_matching_endpoint() {
        let devices = vec![
            device("{speakers}", "Speakers (Realtek Audio)"),
            device("{cable-in}", "CABLE Input (VB-Audio Virtual Cable)"),
        ];
        let mut provider = CableProvider::default();
        assert_eq!(
            provider.ensure(&devices).expect("found"),
            DeviceId::new("{cable-in}")
        );
    }

    #[test]
    fn ensure_reports_missing_cable() {
        let devices = vec![device("{speakers}", "Speakers (Realtek Audio)")];
        let mut provider = CableProvider::default();
        assert_eq!(
            provider.ensure(&devices),
            Err(VirtualError::NotFound("CABLE Input".to_owned()))
        );
    }

    #[test]
    fn release_is_a_no_op() {
        let mut provider = CableProvider::default();
        assert_eq!(provider.release(), Ok(()));
    }
}
