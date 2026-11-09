//! Name-pattern lookup for the virtual device (the Windows provider uses it to find VB-CABLE).

use crate::model::DeviceInfo;

/// Finds the first device whose name contains a pattern, ignoring case.
///
/// The pattern is matched as a plain substring, so `"CABLE Input"` does not match
/// `"CABLE Output"`. This is a helper for providers, not a port.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::module_name_repetitions,
    reason = "the type name `PatternLocator` is fixed by docs/PLAN.md T-3.4"
)]
pub struct PatternLocator {
    pattern: String,
}

impl PatternLocator {
    /// Creates a locator for `pattern`.
    #[must_use]
    pub fn new(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
        }
    }

    /// Returns the pattern this locator searches for.
    #[must_use]
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Returns the first device whose name contains the pattern, ignoring case.
    ///
    /// An empty pattern matches nothing: it would otherwise match the first device, which is
    /// never what a provider wants.
    #[must_use]
    pub fn locate<'a>(&self, devices: &'a [DeviceInfo]) -> Option<&'a DeviceInfo> {
        let needle = self.pattern.to_lowercase();
        if needle.is_empty() {
            return None;
        }
        devices
            .iter()
            .find(|device| device.name.to_lowercase().contains(needle.as_str()))
    }
}

impl Default for PatternLocator {
    /// The pattern of the VB-CABLE playback endpoint that the app routes into.
    fn default() -> Self {
        Self::new("CABLE Input")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DeviceId;

    fn device(id: &str, name: &str) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId::new(id),
            name: name.to_owned(),
            is_virtual: false,
        }
    }

    fn sample_devices() -> Vec<DeviceInfo> {
        vec![
            device("{speakers}", "Speakers (Realtek Audio)"),
            device("{cable-out}", "CABLE Output (VB-Audio Virtual Cable)"),
            device("{cable-in}", "CABLE Input (VB-Audio Virtual Cable)"),
        ]
    }

    #[test]
    fn finds_cable_input() {
        let devices = sample_devices();
        let found = PatternLocator::default().locate(&devices);
        assert_eq!(found.map(|d| d.id.as_str()), Some("{cable-in}"));
    }

    #[test]
    fn ignores_cable_output() {
        let devices = vec![device(
            "{cable-out}",
            "CABLE Output (VB-Audio Virtual Cable)",
        )];
        assert_eq!(PatternLocator::default().locate(&devices), None);
    }

    #[test]
    fn matches_case_insensitively() {
        let devices = sample_devices();
        let found = PatternLocator::new("cable input").locate(&devices);
        assert_eq!(found.map(|d| d.id.as_str()), Some("{cable-in}"));
    }

    #[test]
    fn returns_none_when_absent() {
        let devices = vec![device("{speakers}", "Speakers (Realtek Audio)")];
        assert_eq!(PatternLocator::default().locate(&devices), None);
    }

    #[test]
    fn empty_pattern_matches_nothing() {
        let devices = sample_devices();
        assert_eq!(PatternLocator::new("").locate(&devices), None);
    }
}
