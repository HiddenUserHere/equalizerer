//! Filter design: RBJ Audio EQ Cookbook peaking filter.

use crate::biquad::Coeffs;

/// A band whose centre is at or above this fraction of the sample rate is disabled: it would
/// be too close to Nyquist to be accurate.
pub const MAX_CENTRE_FRACTION: f64 = 0.45;

/// Designs a peaking (bell) EQ band with the RBJ Audio EQ Cookbook formulas.
///
/// `fs` is the sample rate in Hz, `f0` the centre frequency in Hz, `q` the quality factor
/// and `gain_db` the boost (positive) or cut (negative) at `f0`. Returns [`Coeffs::IDENTITY`]
/// when the gain is exactly zero, or when `f0` is at or above [`MAX_CENTRE_FRACTION`] * `fs`.
#[must_use]
pub fn peaking(fs: f64, f0: f64, q: f64, gain_db: f64) -> Coeffs {
    // A zero gain is an exact pass-through; the bit-exact comparison is intentional so that
    // a band at 0 dB skips the filter entirely.
    #[allow(
        clippy::float_cmp,
        reason = "0.0 is an exact sentinel for 'band disabled', not a computed value"
    )]
    let is_flat = gain_db == 0.0;
    if is_flat || f0 >= MAX_CENTRE_FRACTION * fs {
        return Coeffs::IDENTITY;
    }

    let a = libm::pow(10.0, gain_db / 40.0);
    let w0 = 2.0 * core::f64::consts::PI * f0 / fs;
    let cos_w0 = libm::cos(w0);
    let alpha = libm::sin(w0) / (2.0 * q);

    let b0 = 1.0 + alpha * a;
    let b1 = -2.0 * cos_w0;
    let b2 = 1.0 - alpha * a;
    let a0 = 1.0 + alpha / a;
    let a1 = -2.0 * cos_w0;
    let a2 = 1.0 - alpha / a;

    Coeffs {
        b0: b0 / a0,
        b1: b1 / a0,
        b2: b2 / a0,
        a1: a1 / a0,
        a2: a2 / a0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::magnitude_db;
    use approx::assert_abs_diff_eq;

    const SQRT_2: f64 = core::f64::consts::SQRT_2;

    #[test]
    fn zero_gain_is_identity() {
        assert_eq!(peaking(48_000.0, 1_000.0, SQRT_2, 0.0), Coeffs::IDENTITY);
    }

    #[test]
    fn centre_at_or_above_045_fs_is_identity() {
        assert_eq!(peaking(48_000.0, 21_600.0, SQRT_2, 6.0), Coeffs::IDENTITY);
        assert_eq!(peaking(48_000.0, 22_000.0, SQRT_2, 6.0), Coeffs::IDENTITY);
        // Just below the limit the band is designed, not bypassed.
        assert_ne!(peaking(48_000.0, 21_000.0, SQRT_2, 6.0), Coeffs::IDENTITY);
    }

    #[test]
    fn gain_at_centre_matches_request() {
        for &fs in &[44_100.0, 48_000.0, 96_000.0] {
            for &gain in &[-12.0, -3.0, 3.0, 12.0] {
                let c = peaking(fs, 1_000.0, SQRT_2, gain);
                let db = magnitude_db(&c, fs, 1_000.0);
                assert!(
                    (db - gain).abs() <= 0.01,
                    "fs={fs} gain={gain}: measured {db} dB at f0"
                );
            }
        }
    }

    #[test]
    fn band_is_flat_far_from_centre() {
        for &fs in &[44_100.0, 48_000.0, 96_000.0] {
            for &gain in &[-12.0, 12.0] {
                let c = peaking(fs, 1_000.0, SQRT_2, gain);
                assert_abs_diff_eq!(magnitude_db(&c, fs, 10.0), 0.0, epsilon = 0.1);
                assert_abs_diff_eq!(magnitude_db(&c, fs, 0.49 * fs), 0.0, epsilon = 0.1);
            }
        }
    }

    #[test]
    fn coefficients_are_a0_normalised_and_stable() {
        let c = peaking(48_000.0, 1_000.0, SQRT_2, 6.0);
        // Poles of 1 + a1 z^-1 + a2 z^-2 must lie inside the unit circle: |a2| < 1 and
        // |a1| < 1 + a2.
        assert!(c.a2.abs() < 1.0);
        assert!(c.a1.abs() < 1.0 + c.a2);
    }
}
