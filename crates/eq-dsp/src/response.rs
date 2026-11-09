//! Frequency response evaluation of biquad cascades, used for the UI response curve.

use crate::biquad::Coeffs;

/// Lowest frequency of the response curve in Hz.
const CURVE_MIN_HZ: f64 = 20.0;
/// Highest frequency of the response curve in Hz.
const CURVE_MAX_HZ: f64 = 20_000.0;

/// Magnitude of one biquad at frequency `f` (Hz) in decibels, `20 * log10 |H(e^jw)|`.
///
/// Evaluates `H = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2)` at `z = e^{jw}`,
/// `w = 2*pi*f/fs`; see [`response_at`].
#[must_use]
pub fn magnitude_db(c: &Coeffs, fs: f64, f: f64) -> f64 {
    let w = 2.0 * core::f64::consts::PI * f / fs;
    let (re, im) = response_at(
        c,
        libm::cos(w),
        libm::sin(w),
        libm::cos(2.0 * w),
        libm::sin(2.0 * w),
    );
    10.0 * libm::log10(re * re + im * im)
}

/// Complex response `(re, im)` of one biquad, given `cos w`, `sin w`, `cos 2w` and `sin 2w` for
/// the frequency of interest.
///
/// Callers that evaluate many filters at fixed frequencies pass the trigonometry in once. The
/// response of a cascade is the product of the responses, so a band set's summed dB is
/// `10 * log10(re^2 + im^2)` of that product: one logarithm per frequency instead of one per
/// band.
#[must_use]
pub(crate) fn response_at(c: &Coeffs, cos1: f64, sin1: f64, cos2: f64, sin2: f64) -> (f64, f64) {
    // z^-1 = cos w - j sin w and z^-2 = cos 2w - j sin 2w.
    let num_re = c.b0 + c.b1 * cos1 + c.b2 * cos2;
    let num_im = -(c.b1 * sin1 + c.b2 * sin2);
    let den_re = 1.0 + c.a1 * cos1 + c.a2 * cos2;
    let den_im = -(c.a1 * sin1 + c.a2 * sin2);

    // num / den = num * conj(den) / |den|^2.
    let den_sq = den_re * den_re + den_im * den_im;
    (
        (num_re * den_re + num_im * den_im) / den_sq,
        (num_im * den_re - num_re * den_im) / den_sq,
    )
}
/// Frequency of the `i`-th of `n` logarithmically spaced points from `f_min` to `f_max` (Hz).
///
/// Point `i` is `f_min * (f_max / f_min)^(i / (n - 1))`, so point 0 is `f_min` and point
/// `n - 1` is `f_max`. With fewer than two points there is no spacing to speak of and the
/// result is `f_min`.
#[must_use]
pub fn log_frequency(i: usize, n: usize, f_min: f64, f_max: f64) -> f64 {
    if n <= 1 {
        return f_min;
    }
    // Point counts are a few hundred at most, so the conversion to f64 is exact.
    #[allow(
        clippy::cast_precision_loss,
        reason = "point indices are small; usize -> f64 is exact below 2^53"
    )]
    let t = i as f64 / (n - 1) as f64;
    f_min * libm::pow(f_max / f_min, t)
}

/// Writes the summed response of a cascade of biquads into `out`, in dB.
///
/// `out.len()` log-spaced points from 20 Hz to 20 kHz are evaluated; each point is the sum of
/// [`magnitude_db`] over all `coeffs`. Performs no allocation.
pub fn response_curve(coeffs: &[Coeffs], fs: f64, out: &mut [f32]) {
    let n = out.len();
    for (i, slot) in out.iter_mut().enumerate() {
        let f = log_frequency(i, n, CURVE_MIN_HZ, CURVE_MAX_HZ);
        let db: f64 = coeffs.iter().map(|c| magnitude_db(c, fs, f)).sum();
        // The curve is for display; f32 is ample for dB values.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "display curve in dB; f32 precision is far beyond what is drawn"
        )]
        let db_f32 = db as f32;
        *slot = db_f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::peaking;
    use approx::assert_abs_diff_eq;

    const SQRT_2: f64 = core::f64::consts::SQRT_2;

    #[test]
    fn log_frequency_endpoints_and_spacing() {
        assert_abs_diff_eq!(log_frequency(0, 128, 20.0, 20_000.0), 20.0, epsilon = 1e-9);
        assert_abs_diff_eq!(
            log_frequency(127, 128, 20.0, 20_000.0),
            20_000.0,
            epsilon = 1e-6
        );
        // Equal ratios between neighbours: a log grid.
        let r1 = log_frequency(11, 128, 20.0, 20_000.0) / log_frequency(10, 128, 20.0, 20_000.0);
        let r2 = log_frequency(60, 128, 20.0, 20_000.0) / log_frequency(59, 128, 20.0, 20_000.0);
        assert_abs_diff_eq!(r1, r2, epsilon = 1e-12);
    }

    #[test]
    fn log_frequency_single_point_is_f_min() {
        assert_abs_diff_eq!(log_frequency(0, 1, 20.0, 20_000.0), 20.0, epsilon = 1e-12);
    }

    #[test]
    fn identity_has_zero_response() {
        for f in [20.0, 1_000.0, 20_000.0] {
            assert_abs_diff_eq!(
                magnitude_db(&Coeffs::IDENTITY, 48_000.0, f),
                0.0,
                epsilon = 1e-12
            );
        }
    }

    #[test]
    fn magnitude_agrees_with_peaking_design() {
        for &fs in &[44_100.0, 48_000.0, 96_000.0] {
            for &gain in &[-12.0, -3.0, 3.0, 12.0] {
                let c = peaking(fs, 1_000.0, SQRT_2, gain);
                assert_abs_diff_eq!(magnitude_db(&c, fs, 1_000.0), gain, epsilon = 0.01);
            }
        }
    }

    #[test]
    fn two_identical_bands_add_in_db() {
        let fs = 48_000.0;
        let c = peaking(fs, 1_000.0, SQRT_2, 3.0);
        let pair = [c, c];
        let mut curve = [0.0_f32; 16];
        response_curve(&pair, fs, &mut curve);
        for (i, &v) in curve.iter().enumerate() {
            let f = log_frequency(i, curve.len(), CURVE_MIN_HZ, CURVE_MAX_HZ);
            assert_abs_diff_eq!(f64::from(v), 2.0 * magnitude_db(&c, fs, f), epsilon = 1e-4);
        }
        // Summed at f0 itself: +3 dB + +3 dB = +6 dB.
        assert_abs_diff_eq!(magnitude_db(&c, fs, 1_000.0) * 2.0, 6.0, epsilon = 0.02);
    }

    #[test]
    fn response_curve_matches_pointwise_sum() {
        let fs = 48_000.0;
        let bands = [
            peaking(fs, 250.0, SQRT_2, 6.0),
            peaking(fs, 4_000.0, SQRT_2, -9.0),
        ];
        let mut curve = [0.0_f32; 128];
        response_curve(&bands, fs, &mut curve);
        for (i, &v) in curve.iter().enumerate() {
            let f = log_frequency(i, curve.len(), CURVE_MIN_HZ, CURVE_MAX_HZ);
            let want: f64 = bands.iter().map(|c| magnitude_db(c, fs, f)).sum();
            assert_abs_diff_eq!(f64::from(v), want, epsilon = 1e-4);
        }
    }

    #[test]
    fn response_curve_empty_output_is_noop() {
        let mut curve: [f32; 0] = [];
        response_curve(&[Coeffs::IDENTITY], 48_000.0, &mut curve);
    }
}
