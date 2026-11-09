//! The graphic equalizer: one peaking band per layout centre, smoothed gain changes, a
//! dry/wet power crossfade and a final peak limiter.
//!
//! Processing order per frame: bands, then the crossfade with the dry signal, then the limiter.
//! The hot path ([`GraphicEq::process_interleaved`]) never allocates, locks or logs.

use crate::biquad::{Coeffs, StereoBiquad};
use crate::correction::corrected_gains;
use crate::design::peaking;
use crate::layout::{GAIN_MAX_DB, GAIN_MIN_DB, Layout, MAX_BANDS};
use crate::limiter::PeakLimiter;

/// Length of a gain ramp and of the power crossfade, in seconds.
const RAMP_SECONDS: f64 = 0.030;

/// Coefficients of ramping bands are recomputed every this many frames.
const UPDATE_FRAMES: usize = 32;

/// Length of a gain ramp or crossfade in frames: `round(0.030 * fs)`, at least one frame.
#[must_use]
fn ramp_length(fs: f64) -> u32 {
    let frames = libm::round(RAMP_SECONDS * fs);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "fs is a positive sample rate, so the rounded frame count is small and non-negative"
    )]
    let n = frames as u32;
    n.max(1)
}

/// Converts a frame count to `f32`. Frame counts are far below 2^24, so this is exact.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "frame counts are below 2^24, so the conversion to f32 is exact"
)]
fn frames_f32(n: u32) -> f32 {
    n as f32
}

/// Converts a length to `u32`, saturating. Block lengths are at most a few frames here.
#[must_use]
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Narrows a computed `f64` sample to the `f32` audio format.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "audio I/O is f32; rounding to single precision is intended"
)]
fn to_f32(x: f64) -> f32 {
    x as f32
}

/// Exact-zero test for a band gain in dB. Zero is a sentinel for "band flat", not a computed
/// value, so the bit-exact comparison is intended.
#[must_use]
#[allow(
    clippy::float_cmp,
    reason = "0.0 is an exact sentinel for 'band flat', not a computed value"
)]
fn is_flat(db: f32) -> bool {
    db == 0.0
}

/// Stereo graphic equalizer with smoothed gains and a peak limiter.
///
/// Gains are set with [`GraphicEq::set_gains`]. A change does not take effect at once: each
/// band moves linearly in dB over about 30 ms, and its filter coefficients are recomputed every
/// 32 frames, which avoids zipper noise. Power is toggled with [`GraphicEq::set_enabled`], which
/// crossfades between the dry and the processed signal over the same length.
///
/// In accurate mode (the default) the bands run at the gains from
/// [`corrected_gains`], so that the response at each band centre equals its slider.
#[derive(Clone, Debug)]
pub struct GraphicEq {
    /// One stereo filter per band; only the first `layout.len()` are used.
    bands: [StereoBiquad; MAX_BANDS],
    /// Band positions and widths.
    layout: Layout,
    /// Sample rate in Hz.
    fs: f64,
    /// Ramp length in frames for the current sample rate.
    ramp_len: u32,
    /// Slider value of each band in dB, as last set by [`GraphicEq::set_gains`].
    slider_db: [f32; MAX_BANDS],
    /// Whether the bands run at corrected gains (accurate mode).
    accurate: bool,
    /// Gain each band currently runs at, in dB.
    current_db: [f32; MAX_BANDS],
    /// Gain each band is moving towards, in dB.
    target_db: [f32; MAX_BANDS],
    /// Per-frame increment of `current_db` while ramping.
    step_db: [f32; MAX_BANDS],
    /// Frames left in the current ramp of each band; 0 when settled.
    ramp_frames_left: [u32; MAX_BANDS],
    /// Coefficients of the target gains, for drawing the response curve.
    target_coeffs: [Coeffs; MAX_BANDS],
    /// Requested power state.
    enabled: bool,
    /// Wet share of the output: 1.0 processed, 0.0 dry.
    mix: f32,
    /// Value `mix` is moving towards.
    mix_target: f32,
    /// Per-frame increment of `mix` while crossfading.
    mix_step: f32,
    /// Frames left in the crossfade; 0 when settled.
    mix_frames_left: u32,
    /// Indices of the bands that run this block.
    active: [usize; MAX_BANDS],
    /// Number of valid entries in `active`.
    active_len: usize,
    /// Final peak limiter.
    limiter: PeakLimiter,
}

impl GraphicEq {
    /// Creates an enabled, flat equalizer for the sample rate `fs` in Hz.
    #[must_use]
    pub fn new(fs: f64) -> Self {
        let mut eq = Self {
            bands: [StereoBiquad::new(Coeffs::IDENTITY); MAX_BANDS],
            layout: Layout::Ten,
            fs,
            ramp_len: ramp_length(fs),
            slider_db: [0.0; MAX_BANDS],
            accurate: true,
            current_db: [0.0; MAX_BANDS],
            target_db: [0.0; MAX_BANDS],
            step_db: [0.0; MAX_BANDS],
            ramp_frames_left: [0; MAX_BANDS],
            target_coeffs: [Coeffs::IDENTITY; MAX_BANDS],
            enabled: true,
            mix: 1.0,
            mix_target: 1.0,
            mix_step: 0.0,
            mix_frames_left: 0,
            active: [0; MAX_BANDS],
            active_len: 0,
            limiter: PeakLimiter::new(fs),
        };
        eq.rebuild_coefficients();
        eq
    }

    /// Changes the sample rate.
    ///
    /// Ramps in progress restart over the new ramp length from their current value. The limiter
    /// is reset. The corrected gains of accurate mode depend on the sample rate, so they are
    /// recomputed.
    pub fn set_sample_rate(&mut self, fs: f64) {
        self.fs = fs;
        self.ramp_len = ramp_length(fs);
        self.limiter.set_sample_rate(fs);
        self.limiter.reset();

        let ramp = frames_f32(self.ramp_len);
        for (i, left) in self.ramp_frames_left.iter_mut().enumerate() {
            if *left > 0 {
                *left = self.ramp_len;
                self.step_db[i] = (self.target_db[i] - self.current_db[i]) / ramp;
            }
        }
        if self.mix_frames_left > 0 {
            self.mix_frames_left = self.ramp_len;
            self.mix_step = (self.mix_target - self.mix) / ramp;
        }
        self.apply_sliders();
        self.rebuild_coefficients();
    }

    /// Selects the band layout. Gains are kept by band index.
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
        self.rebuild_coefficients();
    }

    /// Sets the gain of each band in dB, in band order. These are the slider values.
    ///
    /// Values are clamped to [`GAIN_MIN_DB`, `GAIN_MAX_DB`]; NaN entries are ignored. Entries
    /// beyond the layout's band count are ignored, and bands without an entry keep their value.
    /// In accurate mode the bands run at the corrected gains, which can differ from the sliders
    /// of neighbouring bands too, so a change to one band may move several bands. Bands whose
    /// target changes start a linear ramp of about 30 ms. Nothing is recomputed when no slider
    /// changes.
    pub fn set_gains(&mut self, gains: &[f32]) {
        let n = self.layout.len();
        let mut changed = false;
        for (slider, &requested) in self.slider_db[..n].iter_mut().zip(gains) {
            if requested.is_nan() {
                continue;
            }
            let value = requested.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
            if value.to_bits() != slider.to_bits() {
                *slider = value;
                changed = true;
            }
        }
        if changed {
            self.apply_sliders();
        }
    }

    /// Turns accurate mode on or off. When it is on, the bands run at the gains from
    /// [`corrected_gains`]; when off, at the slider values. The last sliders are re-applied
    /// at once, so the change takes effect in the same ramp as a gain change.
    pub fn set_accurate(&mut self, accurate: bool) {
        if self.accurate == accurate {
            return;
        }
        self.accurate = accurate;
        self.apply_sliders();
    }

    /// Turns processing on or off.
    ///
    /// Both directions crossfade over about 30 ms. When fully off, the output is the input bit
    /// for bit and the filters are not run.
    pub fn set_enabled(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        self.mix_target = if enabled { 1.0 } else { 0.0 };
        self.mix_step = (self.mix_target - self.mix) / frames_f32(self.ramp_len);
        self.mix_frames_left = self.ramp_len;
    }

    /// Coefficients of the target gains, one per band of the layout. In accurate mode these are
    /// the corrected gains.
    ///
    /// They follow [`GraphicEq::set_gains`] at once, while the audio itself is still ramping.
    #[must_use]
    pub fn coeffs(&self) -> &[Coeffs] {
        &self.target_coeffs[..self.layout.len()]
    }

    /// Processes interleaved stereo samples in place: `[L0, R0, L1, R1, ...]`.
    ///
    /// The length should be even. In debug builds an odd length is an error; in release builds
    /// the trailing sample is left untouched. Performs no allocation.
    pub fn process_interleaved(&mut self, buf: &mut [f32]) {
        debug_assert!(
            buf.len().is_multiple_of(2),
            "interleaved stereo buffer must have an even length"
        );
        let even = buf.len() & !1;
        for block in buf[..even].chunks_mut(2 * UPDATE_FRAMES) {
            self.process_block(block);
        }
    }

    /// Derives the target gains from the sliders (corrected in accurate mode) and starts the
    /// ramps towards them.
    fn apply_sliders(&mut self) {
        let mut targets = self.slider_db;
        if self.accurate {
            let n = self.layout.len();
            corrected_gains(self.layout, self.fs, &self.slider_db[..n], &mut targets);
        }
        self.apply_targets(&targets);
    }

    /// Sets the target of each band that differs from its current target and starts its ramp.
    /// The coefficients of the target gains are updated with it.
    fn apply_targets(&mut self, targets: &[f32; MAX_BANDS]) {
        let ramp = frames_f32(self.ramp_len);
        let fs = self.fs;
        let q = self.layout.q();
        for (i, &centre) in self.layout.centres().iter().enumerate() {
            let target = targets[i];
            if target.to_bits() == self.target_db[i].to_bits() {
                continue;
            }
            self.target_db[i] = target;
            self.step_db[i] = (target - self.current_db[i]) / ramp;
            self.ramp_frames_left[i] = self.ramp_len;
            self.target_coeffs[i] = peaking(fs, centre, q, f64::from(target));
        }
    }

    /// Recomputes the coefficients of every band from the current and target gains.
    fn rebuild_coefficients(&mut self) {
        let fs = self.fs;
        let q = self.layout.q();
        for (i, &centre) in self.layout.centres().iter().enumerate() {
            self.target_coeffs[i] = peaking(fs, centre, q, f64::from(self.target_db[i]));
            self.bands[i].set_coeffs(peaking(fs, centre, q, f64::from(self.current_db[i])));
        }
    }

    /// Advances the band ramps by `frames` frames and updates the coefficients of the bands
    /// that are ramping. Snaps a band to its target when its ramp ends.
    fn advance_ramps(&mut self, frames: u32) {
        let fs = self.fs;
        let q = self.layout.q();
        for (i, &centre) in self.layout.centres().iter().enumerate() {
            let left = self.ramp_frames_left[i];
            if left == 0 {
                continue;
            }
            if frames >= left {
                self.current_db[i] = self.target_db[i];
                self.ramp_frames_left[i] = 0;
            } else {
                self.current_db[i] += self.step_db[i] * frames_f32(frames);
                self.ramp_frames_left[i] = left - frames;
            }
            self.bands[i].set_coeffs(peaking(fs, centre, q, f64::from(self.current_db[i])));
        }
    }

    /// Records which bands run this block and resets the state of the others, so that a band
    /// that is switched on again starts from silence.
    fn select_active_bands(&mut self) {
        let mut len = 0;
        let in_use = self.layout.len();
        let state = self
            .current_db
            .iter()
            .zip(&self.ramp_frames_left)
            .take(in_use);
        for (i, (&db, &left)) in state.enumerate() {
            if left > 0 || !is_flat(db) {
                self.active[len] = i;
                len += 1;
            } else {
                self.bands[i].reset();
            }
        }
        self.active_len = len;
    }

    /// Advances the crossfade by one frame and returns the wet share for that frame.
    fn next_mix(&mut self) -> f32 {
        if self.mix_frames_left > 0 {
            self.mix_frames_left -= 1;
            self.mix = if self.mix_frames_left == 0 {
                self.mix_target
            } else {
                self.mix + self.mix_step
            };
        }
        self.mix
    }

    /// Processes one block of at most `UPDATE_FRAMES` stereo frames.
    fn process_block(&mut self, block: &mut [f32]) {
        self.advance_ramps(to_u32(block.len() / 2));

        if self.mix <= 0.0 && self.mix_frames_left == 0 {
            // Fully bypassed: no filter or limiter state is kept while off.
            for band in &mut self.bands {
                band.reset();
            }
            self.limiter.reset();
            return;
        }

        self.select_active_bands();
        let active_len = self.active_len;

        let (frames, _) = block.as_chunks_mut::<2>();
        for [left, right] in frames {
            let dry = [f64::from(*left), f64::from(*right)];
            let mut wet = dry;
            for &band in &self.active[..active_len] {
                wet = self.bands[band].process(wet);
            }

            let mix = self.next_mix();
            let out = if mix >= 1.0 {
                self.limiter.process(wet)
            } else if mix <= 0.0 {
                // Dry: the limiter only runs when the processed signal is part of the output.
                dry
            } else {
                let m = f64::from(mix);
                let blended = [
                    dry[0] * (1.0 - m) + wet[0] * m,
                    dry[1] * (1.0 - m) + wet[1] * m,
                ];
                self.limiter.process(blended)
            };

            *left = to_f32(out[0]);
            *right = to_f32(out[1]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limiter::CEILING;
    use crate::response::magnitude_db;
    use approx::assert_abs_diff_eq;

    const FS: f64 = 48_000.0;
    /// Index of the 1 kHz band in the ten-band layout.
    const BAND_1K: usize = 5;

    /// Interleaved stereo sine starting at frame `start`.
    fn sine_block(frames: usize, start: usize, amp: f32, f0: f64, fs: f64) -> Vec<f32> {
        let mut out = Vec::with_capacity(2 * frames);
        for n in start..start + frames {
            #[allow(clippy::cast_precision_loss, reason = "test frame indices are small")]
            let t = n as f64 / fs;
            #[allow(
                clippy::cast_possible_truncation,
                reason = "test signal is generated in f32 audio format"
            )]
            let v = libm::sin(2.0 * core::f64::consts::PI * f0 * t) as f32;
            out.push(amp * v);
            out.push(amp * v);
        }
        out
    }

    /// Root-mean-square level of an interleaved buffer.
    #[allow(clippy::cast_precision_loss, reason = "test buffers are small")]
    fn rms(v: &[f32]) -> f64 {
        let sum: f64 = v.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        (sum / v.len() as f64).sqrt()
    }

    /// Largest absolute difference between consecutive samples of one channel.
    fn max_step(buf: &[f32]) -> f32 {
        buf.as_chunks::<2>()
            .0
            .iter()
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    fn gains_with(band: usize, db: f32) -> [f32; 10] {
        let mut g = [0.0; 10];
        g[band] = db;
        g
    }

    #[test]
    fn ramp_length_is_30_ms_in_frames() {
        assert_eq!(GraphicEq::new(48_000.0).ramp_len, 1_440);
        assert_eq!(GraphicEq::new(44_100.0).ramp_len, 1_323);
        assert_eq!(GraphicEq::new(96_000.0).ramp_len, 2_880);
    }

    #[test]
    fn set_sample_rate_updates_ramp_length() {
        let mut eq = GraphicEq::new(48_000.0);
        eq.set_sample_rate(96_000.0);
        assert_eq!(eq.ramp_len, 2_880);
    }

    #[test]
    fn zero_gains_are_bit_exact_passthrough() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&[0.0; 10]);
        let input = sine_block(4_800, 0, 0.5, 1_000.0, FS); // -6 dBFS
        let mut buf = input.clone();
        eq.process_interleaved(&mut buf);
        assert_eq!(buf, input);
    }

    /// Response in dB at `hz` of the summed coefficients of all bands.
    fn response_db(coeffs: &[Coeffs], hz: f64) -> f64 {
        coeffs.iter().map(|c| magnitude_db(c, FS, hz)).sum()
    }

    /// Centre frequencies of the ten-band layout.
    fn centres() -> &'static [f64] {
        Layout::Ten.centres()
    }

    #[test]
    fn set_gains_clamps_to_range() {
        let mut eq = GraphicEq::new(FS);
        eq.set_accurate(false);
        eq.set_gains(&[100.0, -100.0]);
        assert_eq!(eq.target_db[0], GAIN_MAX_DB);
        assert_eq!(eq.target_db[1], GAIN_MIN_DB);
        assert_eq!(eq.slider_db[0], GAIN_MAX_DB);
        assert_eq!(eq.slider_db[1], GAIN_MIN_DB);
    }

    #[test]
    fn set_gains_ignores_nan_and_extra_entries_and_keeps_missing() {
        let mut eq = GraphicEq::new(FS);
        eq.set_accurate(false);
        eq.set_gains(&[3.0, f32::NAN, 4.0]);
        assert_eq!(eq.target_db[0], 3.0);
        assert_eq!(eq.target_db[1], 0.0, "NaN must not change the band");
        assert_eq!(eq.target_db[2], 4.0);

        // Missing entries keep their value.
        eq.set_gains(&[]);
        assert_eq!(eq.target_db[0], 3.0);

        // Entries beyond the ten bands are ignored.
        eq.set_gains(&[0.0; 10]);
        eq.set_gains(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 5.0]);
        assert_eq!(eq.target_db[10], 0.0);
        assert_eq!(eq.slider_db[10], 0.0);
    }

    #[test]
    fn ramp_is_linear_and_reaches_target_after_ramp_length() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&gains_with(BAND_1K, 6.0));
        // The corrected target of the band; it is not exactly the slider in accurate mode.
        let target = eq.target_db[BAND_1K];

        let mut buf = sine_block(720, 0, 0.1, 1_000.0, FS);
        eq.process_interleaved(&mut buf);
        assert_abs_diff_eq!(eq.current_db[BAND_1K], target / 2.0, epsilon = 0.05);

        let mut buf = sine_block(720, 720, 0.1, 1_000.0, FS);
        eq.process_interleaved(&mut buf);
        assert_eq!(eq.current_db[BAND_1K], target);
        assert_eq!(eq.ramp_frames_left[BAND_1K], 0);
    }

    #[test]
    fn steady_boost_matches_request_at_1khz() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&gains_with(BAND_1K, 6.0));
        let warm = sine_block(4_800, 0, 0.1, 1_000.0, FS);
        let mut buf = warm;
        eq.process_interleaved(&mut buf);

        let input = sine_block(2_400, 4_800, 0.1, 1_000.0, FS);
        let mut out = input.clone();
        eq.process_interleaved(&mut out);

        let gain_db = 20.0 * (rms(&out) / rms(&input)).log10();
        assert!((gain_db - 6.0).abs() < 0.2, "measured {gain_db} dB");
    }

    #[test]
    fn coeffs_sum_to_the_sliders_at_every_centre() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&gains_with(BAND_1K, 6.0));
        let c = eq.coeffs();
        assert_eq!(c.len(), 10);
        for (band, &hz) in centres().iter().enumerate() {
            let expected = f64::from(eq.slider_db[band]);
            let measured = response_db(c, hz);
            assert!(
                (measured - expected).abs() < 0.5,
                "centre {hz} Hz: {measured} dB, slider {expected} dB"
            );
        }
    }

    #[test]
    fn coeffs_follow_slider_gains_without_correction() {
        let mut eq = GraphicEq::new(FS);
        eq.set_accurate(false);
        eq.set_gains(&gains_with(BAND_1K, 12.0));
        let c = eq.coeffs();
        assert_eq!(c.len(), 10);
        assert!((magnitude_db(&c[BAND_1K], FS, 1_000.0) - 12.0).abs() < 0.01);
        assert_eq!(c[0], Coeffs::IDENTITY);
    }

    #[test]
    fn accurate_mode_passes_the_response_through_the_sliders() {
        let mut eq = GraphicEq::new(FS);
        let mut sliders = [0.0_f32; 10];
        sliders[0] = 10.0;
        sliders[1] = -12.0;
        eq.set_gains(&sliders);
        let c = eq.coeffs();
        for (&hz, &slider) in centres().iter().zip(&sliders) {
            let err = (response_db(c, hz) - f64::from(slider)).abs();
            assert!(err <= 0.5, "centre {hz} Hz: error {err} dB");
        }
    }

    #[test]
    fn toggling_accurate_reapplies_the_last_sliders() {
        let mut sliders = [0.0_f32; 10];
        sliders[0] = 10.0;
        sliders[1] = -12.0;

        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&sliders);
        let corrected = eq.target_db;
        assert_ne!(
            corrected[0], 10.0,
            "the correction should move the 32 Hz band"
        );

        eq.set_accurate(false);
        assert_eq!(eq.target_db[0], 10.0);
        assert_eq!(eq.target_db[1], -12.0);

        eq.set_accurate(true);
        assert_eq!(eq.target_db, corrected);
    }

    #[test]
    fn zero_to_plus_12_db_step_has_no_click() {
        let amp = 0.1_f32;
        let mut eq = GraphicEq::new(FS);
        let mut buf = sine_block(2_000, 0, amp, 1_000.0, FS);
        eq.process_interleaved(&mut buf);

        eq.set_gains(&gains_with(BAND_1K, 12.0));
        let mut tail = sine_block(4_800, 2_000, amp, 1_000.0, FS);
        eq.process_interleaved(&mut tail);

        // Largest slope of the boosted 1 kHz sine: 0.1 * 10^(12/20) * omega.
        let peak = f64::from(amp) * 10_f64.powf(12.0 / 20.0);
        let max_slope = peak * 2.0 * core::f64::consts::PI * 1_000.0 / FS;
        let step = f64::from(max_step(&tail).max(max_step(&buf)));
        assert!(step <= 1.5 * max_slope, "step {step} > {}", 1.5 * max_slope);
    }

    #[test]
    fn disable_fades_to_bit_exact_passthrough() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&gains_with(BAND_1K, 6.0));
        let mut buf = sine_block(4_800, 0, 0.1, 1_000.0, FS);
        eq.process_interleaved(&mut buf);

        eq.set_enabled(false);
        let mut fade = sine_block(2_880, 4_800, 0.1, 1_000.0, FS);
        eq.process_interleaved(&mut fade);

        let input = sine_block(480, 7_680, 0.1, 1_000.0, FS);
        let mut out = input.clone();
        eq.process_interleaved(&mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn toggling_enabled_has_no_click() {
        let amp = 0.1_f32;
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&gains_with(BAND_1K, 12.0));
        let mut buf = sine_block(4_800, 0, amp, 1_000.0, FS);
        eq.process_interleaved(&mut buf);

        eq.set_enabled(false);
        let mut off = sine_block(2_880, 4_800, amp, 1_000.0, FS);
        eq.process_interleaved(&mut off);
        eq.set_enabled(true);
        let mut on = sine_block(2_880, 7_680, amp, 1_000.0, FS);
        eq.process_interleaved(&mut on);

        let peak = f64::from(amp) * 10_f64.powf(12.0 / 20.0);
        let max_slope = peak * 2.0 * core::f64::consts::PI * 1_000.0 / FS;
        let step = f64::from(max_step(&off).max(max_step(&on)));
        assert!(step <= 1.5 * max_slope, "step {step} > {}", 1.5 * max_slope);
    }

    #[test]
    fn output_never_exceeds_ceiling_with_heavy_boost() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&[12.0; 10]);
        let mut buf = sine_block(9_600, 0, 0.5, 1_000.0, FS);
        eq.process_interleaved(&mut buf);
        for &x in &buf {
            assert!(
                f64::from(x.abs()) <= CEILING + 1e-6,
                "sample {x} above ceiling"
            );
        }
    }

    #[test]
    fn empty_buffer_is_noop() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&gains_with(BAND_1K, 6.0));
        let mut buf: Vec<f32> = Vec::new();
        eq.process_interleaved(&mut buf);
        assert_eq!(buf, Vec::<f32>::new());
    }

    #[test]
    fn set_layout_keeps_gains_and_coeff_count() {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&gains_with(BAND_1K, 6.0));
        eq.set_layout(Layout::Ten);
        assert_eq!(eq.coeffs().len(), 10);
        assert_eq!(eq.slider_db[BAND_1K], 6.0);
    }
}
