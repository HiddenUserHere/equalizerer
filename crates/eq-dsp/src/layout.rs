//! Band layouts: where the EQ bands sit and how wide they are.

/// Lower bound of a band gain in dB.
pub const GAIN_MIN_DB: f32 = -12.0;

/// Upper bound of a band gain in dB.
pub const GAIN_MAX_DB: f32 = 12.0;

/// Capacity of the band arrays. Layouts with fewer bands use a prefix of this capacity.
pub const MAX_BANDS: usize = 31;

// The gain range must be non-empty; checked at compile time.
const _: () = assert!(GAIN_MIN_DB < GAIN_MAX_DB);

/// Centre frequencies of the 10-band layout in Hz (one-octave spacing, 32 Hz to 16 kHz).
const TEN_CENTRES_HZ: [f64; 10] = [
    32.0, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

/// A fixed set of EQ band positions.
///
/// It is an enum so that more layouts (for example a 31-band one) can be added without
/// changing the callers. Only [`Layout::Ten`] exists for the MVP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Ten bands at octave centres from 32 Hz to 16 kHz.
    Ten,
}

impl Layout {
    /// Centre frequencies of the bands in Hz, strictly increasing.
    #[must_use]
    pub const fn centres(self) -> &'static [f64] {
        match self {
            Self::Ten => &TEN_CENTRES_HZ,
        }
    }

    /// Quality factor shared by all bands. `sqrt(2)` gives one-octave bandwidth.
    #[must_use]
    pub const fn q(self) -> f64 {
        match self {
            Self::Ten => core::f64::consts::SQRT_2,
        }
    }

    /// Number of bands in this layout.
    #[must_use]
    pub const fn len(self) -> usize {
        self.centres().len()
    }

    /// Returns `true` if the layout has no bands. No current layout is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_band_centres_are_strictly_increasing_and_match_architecture() {
        let c = Layout::Ten.centres();
        assert_eq!(c.len(), 10);
        assert_eq!(
            c,
            &[
                32.0, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0
            ]
        );
        for pair in c.windows(2) {
            assert!(pair[0] < pair[1], "centres not increasing: {pair:?}");
        }
    }

    #[test]
    fn ten_band_q_is_sqrt_two() {
        assert!((Layout::Ten.q() - core::f64::consts::SQRT_2).abs() < f64::EPSILON);
    }

    #[test]
    fn len_matches_centres_and_capacity() {
        assert_eq!(Layout::Ten.len(), 10);
        assert!(!Layout::Ten.is_empty());
        assert!(Layout::Ten.len() <= MAX_BANDS);
    }

    #[test]
    fn gain_limits_are_symmetric() {
        assert!((GAIN_MIN_DB + GAIN_MAX_DB).abs() < f32::EPSILON);
    }
}
