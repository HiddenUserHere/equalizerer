//! Output endpoint enumeration through `IMMDeviceEnumerator`.

use eq_core::{BackendError, DeviceId, DeviceInfo, EndpointDirectory, PatternLocator};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::BSTR;

use crate::com::{ComApartment, pwstr_to_string};
use crate::error::{ComResultExt, HRESULT_NOT_FOUND, WasapiError};

/// Lists output endpoints and reads the system default, using the `MMDevice` API.
///
/// The directory is bound to the thread that created it and must be used inside the
/// [`ComApartment`] it was created with. It is neither `Send` nor `Sync`.
pub struct MmDirectory {
    enumerator: IMMDeviceEnumerator,
    locator: PatternLocator,
}

impl MmDirectory {
    /// Creates the device enumerator. `locator` decides which endpoint is marked as virtual.
    ///
    /// The `apartment` argument is proof that COM is initialized on this thread; it is not used
    /// otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`WasapiError::Com`] when the `MMDevice` enumerator cannot be created, for example
    /// when the Windows audio service is not running.
    pub fn new(_apartment: &ComApartment, locator: PatternLocator) -> Result<Self, WasapiError> {
        // SAFETY: `MMDeviceEnumerator` is a valid class id and the outer object is null, as the
        // API requires. The apartment argument proves COM is initialized on this thread.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
                .context("CoCreateInstance(MMDeviceEnumerator)")?;
        Ok(Self {
            enumerator,
            locator,
        })
    }

    /// Returns the underlying enumerator, for [`crate::NotificationGuard::register`].
    #[must_use]
    pub fn enumerator(&self) -> &IMMDeviceEnumerator {
        &self.enumerator
    }

    fn enumerate_render(&self) -> Result<Vec<DeviceInfo>, WasapiError> {
        // SAFETY: plain COM call on the enumerator owned by this directory. The flags are
        // valid constants.
        let collection = unsafe {
            self.enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
        }
        .context("IMMDeviceEnumerator::EnumAudioEndpoints")?;
        // SAFETY: plain COM call on a collection we own.
        let count = unsafe { collection.GetCount() }.context("IMMDeviceCollection::GetCount")?;

        let mut devices = Vec::new();
        for index in 0..count {
            // SAFETY: `index` is below the count returned by `GetCount`.
            let device = unsafe { collection.Item(index) }.context("IMMDeviceCollection::Item")?;
            let id = endpoint_id(&device)?;
            let name = friendly_name(&device, &id)?;
            devices.push(DeviceInfo {
                id: DeviceId::new(id),
                name,
                is_virtual: false,
            });
        }
        mark_virtual(&mut devices, &self.locator);
        Ok(devices)
    }

    fn default_id(&self) -> Result<Option<DeviceId>, WasapiError> {
        // SAFETY: plain COM call on the enumerator owned by this directory. The flags are
        // valid constants.
        match unsafe { self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
            Ok(device) => Ok(Some(DeviceId::new(endpoint_id(&device)?))),
            Err(err) if err.code() == HRESULT_NOT_FOUND => Ok(None),
            Err(source) => Err(WasapiError::Com {
                context: "IMMDeviceEnumerator::GetDefaultAudioEndpoint",
                source,
            }),
        }
    }
}

impl EndpointDirectory for MmDirectory {
    fn list_render(&self) -> Result<Vec<DeviceInfo>, BackendError> {
        self.enumerate_render().map_err(BackendError::from)
    }

    fn default_render(&self) -> Result<Option<DeviceId>, BackendError> {
        self.default_id().map_err(BackendError::from)
    }

    fn set_default_render(&self, id: &DeviceId) -> Result<(), BackendError> {
        crate::policy_config::set_default_endpoint(id).map_err(BackendError::from)
    }
}

/// Marks the first device whose name matches `locator` as virtual and leaves the others unmarked.
///
/// Only one device is marked, because the locator returns the first match and the controller
/// routes into exactly one device.
pub(crate) fn mark_virtual(devices: &mut [DeviceInfo], locator: &PatternLocator) {
    let virtual_id = locator.locate(devices).map(|device| device.id.clone());
    for device in devices {
        device.is_virtual = virtual_id.as_ref() == Some(&device.id);
    }
}

/// Reads the endpoint id of `device`.
fn endpoint_id(device: &IMMDevice) -> Result<String, WasapiError> {
    // SAFETY: plain COM call. The returned string is COM-allocated and is handed to
    // `pwstr_to_string`, which frees it exactly once.
    let raw = unsafe { device.GetId() }.context("IMMDevice::GetId")?;
    // SAFETY: `raw` was returned by `GetId`, is owned by us and is not used after this call.
    Ok(unsafe { pwstr_to_string(raw) })
}

/// Reads the friendly name of `device`, falling back to `id` when the property is missing or
/// is not a string.
fn friendly_name(device: &IMMDevice, id: &str) -> Result<String, WasapiError> {
    // SAFETY: plain COM call with a valid access mode. The returned store is owned by us.
    let store: IPropertyStore =
        unsafe { device.OpenPropertyStore(STGM_READ) }.context("IMMDevice::OpenPropertyStore")?;
    // SAFETY: `PKEY_Device_FriendlyName` is a static key that outlives the call. The returned
    // PROPVARIANT is owned by us and cleared when dropped.
    let value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }
        .context("IPropertyStore::GetValue(PKEY_Device_FriendlyName)")?;
    Ok(BSTR::try_from(&value).map_or_else(|_| id.to_owned(), |name| name.to_string()))
}

#[cfg(test)]
mod tests {
    use eq_core::{DeviceId, DeviceInfo, EndpointDirectory, PatternLocator};

    use super::*;

    fn device(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: name.to_owned(),
            is_virtual: false,
        }
    }

    #[test]
    fn marks_only_the_first_match_as_virtual() {
        let mut devices = vec![
            device("{speakers}", "Speakers (Realtek Audio)"),
            device("{cable-in}", "CABLE Input (VB-Audio Virtual Cable)"),
            device("{cable-in-2}", "CABLE Input (second)"),
        ];
        mark_virtual(&mut devices, &PatternLocator::default());
        let flags: Vec<bool> = devices.iter().map(|d| d.is_virtual).collect();
        assert_eq!(flags, [false, true, false]);
    }

    #[test]
    fn marks_nothing_without_a_match() {
        let mut devices = vec![device("{speakers}", "Speakers (Realtek Audio)")];
        mark_virtual(&mut devices, &PatternLocator::default());
        assert!(!devices[0].is_virtual);
    }

    #[test]
    fn enumerates_render_endpoints() {
        let apartment = ComApartment::init_mta().expect("COM");
        let directory =
            MmDirectory::new(&apartment, PatternLocator::default()).expect("MMDevice enumerator");
        let devices = directory.list_render().expect("list_render");
        // CI machines may have no output device, so the list may be empty. Each device must
        // still have an id and a name.
        for device in &devices {
            assert_ne!(device.id.as_str(), "");
            assert_ne!(device.name, "");
        }
        directory.default_render().expect("default_render");
    }
}
