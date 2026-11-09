//! Band-interaction correction for the graphic EQ ("accurate mode").
//!
//! Neighbouring peaking bands overlap, so the summed response at a band's centre differs from
//! that band's slider value: +10 dB at 32 Hz next to -12 dB at 63 Hz gives only about +7.5 dB
//! at 32 Hz. [`corrected_gains`] finds band gains whose combined response at every centre equals
//! its slider. The filters run at the corrected gains and the response curve is drawn from them,
//! so the curve passes through the slider handles.
//!
//! The solver is a fixed-point iteration: measure the response at every centre, move each band
//! by the error at its own centre, and repeat. It never allocates; all state lives in fixed-size
//! arrays of [`MAX_BANDS`].

use crate::biquad::Coeffs;
use crate::design::{MAX_CENTRE_FRACTION, peaking};
use crate::layout::{GAIN_MAX_DB, GAIN_MIN_DB, Layout, MAX_BANDS};
use crate::response::response_at;

/// Number of correction iterations.
///
/// Measured with the test cases: the random and structured sliders have converged to their
/// final error after 8 iterations, and 32 iterations change nothing measurable. Each iteration
/// costs `N` filter designs and `N * N` filter evaluations for an `N`-band layout.
pub const ITERATIONS: usize = 8;

/// Largest magnitude of a corrected gain in dB.
///
/// This caps the search. Sliders that alternate between the extremes (+12, -12, +12, ...) need
/// corrections beyond this cap, so their response stays up to about 1.2 dB off the sliders.
pub const CORRECTION_LIMIT_DB: f32 = 18.0;

/// Converts a slider entry to the clamped gain used by the solver. A missing or NaN entry is
/// treated as 0 dB, so the result is always finite.
#[must_use]
fn slider_gain(entry: Option<&f32>) -> f64 {
    match entry {
        Some(&db) if !db.is_nan() => f64::from(db.clamp(GAIN_MIN_DB, GAIN_MAX_DB)),
        _ => 0.0,
    }
}

/// Computes the band gains that make the combined response at every band centre equal the
/// slider value, and writes them to `out` in band order.
///
/// `sliders` holds one gain in dB per band of `layout`, in band order. Values are clamped to
/// the slider range; a missing entry or NaN counts as 0 dB. Bands whose centre is at or above
/// [`MAX_CENTRE_FRACTION`] of `fs` are disabled by the filter design, so they cannot be
/// corrected: their output is their clamped slider value. Entries of `out` beyond the layout's
/// band count are set to 0.
///
/// The corrected gains stay within ±[`CORRECTION_LIMIT_DB`]. The function performs no
/// allocation.
#[allow(
    clippy::cast_possible_truncation,
    reason = "corrected gains are dB values in the single-precision audio format"
)]
pub fn corrected_gains(layout: Layout, fs: f64, sliders: &[f32], out: &mut [f32; MAX_BANDS]) {
    let centres = layout.centres();
    let q = layout.q();
    let n = centres.len();
    let limit = f64::from(CORRECTION_LIMIT_DB);

    // Per band: the slider target, whether the band is designed at this rate, and the cosines
    // and sines of its centre frequency at `w` and `2w`, reused by every iteration.
    let mut target = [0.0_f64; MAX_BANDS];
    let mut enabled = [false; MAX_BANDS];
    let mut trig = [(0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64); MAX_BANDS];
    for (i, &centre) in centres.iter().enumerate() {
        target[i] = slider_gain(sliders.get(i));
        enabled[i] = centre < MAX_CENTRE_FRACTION * fs;
        let w = 2.0 * core::f64::consts::PI * centre / fs;
        trig[i] = (
            libm::cos(w),
            libm::sin(w),
            libm::cos(2.0 * w),
            libm::sin(2.0 * w),
        );
    }

    // Jacobi-style update: every band moves by the error measured with the previous gains.
    let mut gain = target;
    for _ in 0..ITERATIONS {
        let mut filters = [Coeffs::IDENTITY; MAX_BANDS];
        for (j, filter) in filters.iter_mut().enumerate().take(n) {
            if enabled[j] {
                *filter = peaking(fs, centres[j], q, gain[j]);
            }
        }

        let mut next = gain;
        for (i, next_gain) in next.iter_mut().enumerate().take(n) {
            if !enabled[i] {
                continue;
            }
            let (cos1, sin1, cos2, sin2) = trig[i];
            // The summed dB of the bands is the dB of their product, so one logarithm suffices.
            // Disabled bands are identity filters and leave the product unchanged.
            let (mut re, mut im) = (1.0_f64, 0.0_f64);
            for c in &filters[..n] {
                let (hr, hi) = response_at(c, cos1, sin1, cos2, sin2);
                (re, im) = (re * hr - im * hi, re * hi + im * hr);
            }
            let measured = 10.0 * libm::log10(re * re + im * im);
            *next_gain = (gain[i] + (target[i] - measured)).clamp(-limit, limit);
        }
        gain = next;
    }

    out.fill(0.0);
    for (slot, &g) in out.iter_mut().zip(gain.iter()).take(n) {
        *slot = g as f32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::magnitude_db;

    const FS: f64 = 48_000.0;
    const CASES: usize = 2_000;

    /// Deterministic generator (`SplitMix64`) so the random cases are reproducible.
    struct Rng(u64);

    impl Rng {
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        /// A slider value in 0.1 dB steps across the slider range (241 values, -12.0 to 12.0).
        fn slider(&mut self) -> f32 {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_precision_loss,
                reason = "the modulo keeps the value in 0..=240, exact in f32"
            )]
            let k = (self.next_u64() % 241) as f32;
            GAIN_MIN_DB + k / 10.0
        }

        fn sliders(&mut self) -> [f32; 10] {
            let mut s = [0.0_f32; 10];
            for v in &mut s {
                *v = self.slider();
            }
            s
        }
    }

    /// Response in dB at each centre of `layout` for the given band gains, evaluated with the
    /// public [`magnitude_db`] so the test does not share the solver's arithmetic.
    fn measured_at_centres(layout: Layout, fs: f64, gains: &[f32]) -> Vec<f64> {
        let q = layout.q();
        let filters: Vec<Coeffs> = layout
            .centres()
            .iter()
            .zip(gains)
            .map(|(&f0, &g)| peaking(fs, f0, q, f64::from(g)))
            .collect();
        layout
            .centres()
            .iter()
            .map(|&hz| filters.iter().map(|c| magnitude_db(c, fs, hz)).sum())
            .collect()
    }

    /// Largest |measured - slider| over all centres of the ten-band layout, in dB.
    fn worst_error(sliders: &[f32]) -> f64 {
        let mut out = [0.0_f32; MAX_BANDS];
        corrected_gains(Layout::Ten, FS, sliders, &mut out);
        let measured = measured_at_centres(Layout::Ten, FS, &out[..10]);
        measured
            .iter()
            .zip(sliders)
            .map(|(m, &s)| (m - f64::from(s)).abs())
            .fold(0.0, f64::max)
    }

    #[test]
    fn flat_sliders_give_exactly_flat_output() {
        let mut out = [1.0_f32; MAX_BANDS];
        corrected_gains(Layout::Ten, FS, &[0.0; 10], &mut out);
        for (i, g) in out.iter().enumerate() {
            assert_eq!(g.to_bits(), 0.0_f32.to_bits(), "band {i} is {g}");
        }
    }

    #[test]
    fn entries_beyond_the_layout_are_zero() {
        let mut out = [7.0_f32; MAX_BANDS];
        corrected_gains(Layout::Ten, FS, &[1.0; 10], &mut out);
        assert!(out[10..].iter().all(|&g| g == 0.0));
    }

    #[test]
    fn boost_next_to_cut_passes_through_the_handles() {
        let sliders = [10.0, -12.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let err = worst_error(&sliders);
        assert!(err <= 0.5, "worst error {err} dB");
    }

    #[test]
    fn random_sliders_match_within_half_a_db() {
        let mut rng = Rng(0x5EED_0001);
        let mut worst = 0.0_f64;
        for _ in 0..CASES {
            worst = worst.max(worst_error(&rng.sliders()));
        }
        eprintln!("random sliders: worst error over {CASES} cases = {worst:.6} dB");
        assert!(worst <= 0.5, "worst error {worst} dB");
    }

    #[test]
    fn structured_sliders_match_within_half_a_db() {
        let patterns: [[f32; 10]; 5] = [
            [12.0; 10],
            [-12.0; 10],
            [0.0, 12.0, 0.0, 0.0, -12.0, 0.0, 0.0, 12.0, 0.0, 0.0],
            [12.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -12.0],
            [0.0, 0.0, 0.0, 0.0, 12.0, -12.0, 0.0, 0.0, 0.0, 0.0],
        ];
        for sliders in &patterns {
            let err = worst_error(sliders);
            assert!(err <= 0.5, "{sliders:?}: worst error {err} dB");
        }
    }

    #[test]
    fn alternating_extremes_hit_the_limit() {
        // Neighbours at +12 and -12 need corrections beyond the cap, so the match is not exact.
        // This pins the documented residual rather than hiding it.
        let sliders = [
            12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0,
        ];
        let mut out = [0.0_f32; MAX_BANDS];
        corrected_gains(Layout::Ten, FS, &sliders, &mut out);
        assert!(
            out[..10]
                .iter()
                .any(|g| g.abs() >= CORRECTION_LIMIT_DB - 1e-3)
        );
        let err = worst_error(&sliders);
        assert!(err <= 1.2, "worst error {err} dB");
    }

    #[test]
    fn corrected_gains_stay_within_the_limit() {
        let mut rng = Rng(0x5EED_0002);
        for _ in 0..CASES {
            let mut out = [0.0_f32; MAX_BANDS];
            corrected_gains(Layout::Ten, FS, &rng.sliders(), &mut out);
            for g in &out[..10] {
                assert!(g.abs() <= CORRECTION_LIMIT_DB, "gain {g} beyond the limit");
            }
        }
    }

    #[test]
    fn disabled_bands_keep_their_clamped_slider() {
        // At 30 kHz the 16 kHz band (centre >= 0.45 * fs) cannot be designed.
        let mut sliders = [0.0_f32; 10];
        sliders[9] = 20.0;
        let mut out = [0.0_f32; MAX_BANDS];
        corrected_gains(Layout::Ten, 30_000.0, &sliders, &mut out);
        assert_eq!(out[9], GAIN_MAX_DB);
    }

    #[test]
    fn nan_and_missing_entries_count_as_flat() {
        let mut with_nan = [0.0_f32; MAX_BANDS];
        let mut with_zero = [0.0_f32; MAX_BANDS];
        corrected_gains(Layout::Ten, FS, &[f32::NAN, 3.0], &mut with_nan);
        corrected_gains(Layout::Ten, FS, &[0.0, 3.0], &mut with_zero);
        assert_eq!(with_nan, with_zero);
        assert!(with_nan.iter().all(|g| g.is_finite()));
    }
}
