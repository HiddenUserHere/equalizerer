//! View model of the output dropdown: the popup rows, the text the control shows, whether it can
//! be used and which device an entry index stands for.
//!
//! Pure functions only, so the rules are covered by unit tests.

use eq_core::{DeviceId, DeviceInfo};

/// Text of the dropdown when there is no output device to choose from.
pub const NO_DEVICES_LABEL: &str = "No output devices";

/// Text of the dropdown when devices exist but none of them is the target.
pub const NO_SELECTION_LABEL: &str = "Choose output";

/// One line of the output popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// Device name shown in the popup.
    pub name: String,
    /// `true` for the device that is currently the target, which is highlighted.
    pub active: bool,
}

/// Builds the popup list in the order of `devices`. The target is marked active.
#[must_use]
pub fn build_rows(devices: &[DeviceInfo], target: Option<&DeviceId>) -> Vec<DeviceRow> {
    devices
        .iter()
        .map(|device| DeviceRow {
            name: device.name.clone(),
            active: Some(&device.id) == target,
        })
        .collect()
}

/// Text of the dropdown: the target's name, or a hint when there is no device or no target.
#[must_use]
pub fn selector_label(devices: &[DeviceInfo], target: Option<&DeviceId>) -> String {
    if devices.is_empty() {
        return NO_DEVICES_LABEL.to_owned();
    }
    target
        .and_then(|id| devices.iter().find(|device| device.id == *id))
        .map_or_else(
            || NO_SELECTION_LABEL.to_owned(),
            |device| device.name.clone(),
        )
}

/// Whether the dropdown can be opened. An empty list leaves it disabled.
#[must_use]
pub const fn is_enabled(devices: &[DeviceInfo]) -> bool {
    !devices.is_empty()
}

/// The device that popup entry `index` stands for, if there is one.
#[must_use]
pub fn device_at(devices: &[DeviceInfo], index: usize) -> Option<DeviceId> {
    devices.get(index).map(|device| device.id.clone())
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

    fn sample() -> Vec<DeviceInfo> {
        vec![
            device("speakers", "Speakers"),
            device("headphones", "Headphones"),
        ]
    }

    #[test]
    fn rows_follow_device_order_and_mark_the_target() {
        let devices = sample();
        let rows = build_rows(&devices, Some(&DeviceId::new("headphones")));
        assert_eq!(
            rows,
            vec![
                DeviceRow {
                    name: "Speakers".to_owned(),
                    active: false,
                },
                DeviceRow {
                    name: "Headphones".to_owned(),
                    active: true,
                },
            ]
        );
    }

    #[test]
    fn no_row_is_active_without_a_target() {
        let rows = build_rows(&sample(), None);
        assert!(rows.iter().all(|row| !row.active));
    }

    #[test]
    fn label_shows_the_target_name() {
        let devices = sample();
        assert_eq!(
            selector_label(&devices, Some(&DeviceId::new("speakers"))),
            "Speakers"
        );
    }

    #[test]
    fn label_asks_for_a_choice_when_the_target_is_unknown() {
        let devices = sample();
        assert_eq!(selector_label(&devices, None), NO_SELECTION_LABEL);
        assert_eq!(
            selector_label(&devices, Some(&DeviceId::new("gone"))),
            NO_SELECTION_LABEL
        );
    }

    #[test]
    fn empty_list_disables_the_control_and_says_so() {
        assert_eq!(selector_label(&[], None), NO_DEVICES_LABEL);
        assert_eq!(
            selector_label(&[], Some(&DeviceId::new("speakers"))),
            NO_DEVICES_LABEL
        );
        assert!(!is_enabled(&[]));
        assert!(is_enabled(&sample()));
    }

    #[test]
    fn index_maps_to_the_device_at_that_position() {
        let devices = sample();
        assert_eq!(device_at(&devices, 0), Some(DeviceId::new("speakers")));
        assert_eq!(device_at(&devices, 1), Some(DeviceId::new("headphones")));
        assert_eq!(device_at(&devices, 2), None);
        assert_eq!(device_at(&[], 0), None);
    }
}
