//! Parameters shared between the controller and the engine.
//!
//! The type lives in `eq-dsp` because both `eq-core` and `eq-engine` use it and neither
//! depends on the other.

use crate::layout::{Layout, MAX_BANDS};

/// Snapshot of the equalizer settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqParams {
    /// Band layout; decides how many entries of `gains` are in use.
    pub layout: Layout,
    /// Gain per band in dB. Only the first `layout.len()` entries are meaningful.
    pub gains: [f32; MAX_BANDS],
    /// Whether the EQ is processing at all (the power button).
    pub enabled: bool,
}

impl Default for EqParams {
    /// A flat, enabled 10-band EQ: all gains 0 dB.
    fn default() -> Self {
        Self {
            layout: Layout::Ten,
            gains: [0.0; MAX_BANDS],
            enabled: true,
        }
    }
}

impl EqParams {
    /// The gains that belong to the bands of the current layout.
    #[must_use]
    pub fn band_gains(&self) -> &[f32] {
        &self.gains[..self.layout.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_flat_and_enabled() {
        let p = EqParams::default();
        assert_eq!(p.layout, Layout::Ten);
        assert!(p.enabled);
        assert!(p.gains.iter().all(|&g| g == 0.0));
    }

    #[test]
    fn band_gains_has_layout_length() {
        let mut p = EqParams::default();
        p.gains[9] = 4.5;
        p.gains[10] = 7.0;
        let bands = p.band_gains();
        assert_eq!(bands.len(), Layout::Ten.len());
        assert_eq!(bands.len(), 10);
        assert_eq!(bands[9], 4.5);
    }
}
