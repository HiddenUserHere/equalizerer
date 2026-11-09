//! Stereo-linked peak limiter: instant attack, slow release, output ceiling at -0.3 dBFS.
//!
//! The limiter is the last stage of the graphic EQ. It never allocates and does a fixed amount
//! of work per frame, so it is safe to run on the real-time audio thread.

/// Output ceiling of the limiter as a linear amplitude: `10^(-0.3/20)`, about -0.3 dBFS.
pub const CEILING: f64 = 0.966_050_878_989_813_3;

/// Release time constant in seconds: the gain recovers about 63 % of the way in this time.
const RELEASE_SECONDS: f64 = 0.1;

/// Per-frame smoothing coefficient of the release, `1 - exp(-1 / (0.1 * fs))`.
#[must_use]
fn release_coeff(fs: f64) -> f64 {
    1.0 - libm::exp(-1.0 / (RELEASE_SECONDS * fs))
}

/// Stereo-linked peak limiter.
///
/// One gain value is shared by both channels, so the stereo image is preserved. When a frame
/// would exceed [`CEILING`] the gain drops to the value that brings the peak exactly to the
/// ceiling, immediately. Afterwards the gain returns towards 1.0 with a first-order release of
/// about 100 ms. The gain never rises above the value required by the current frame, so the
/// output never exceeds the ceiling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PeakLimiter {
    /// Current gain, in `(0, 1]`.
    gain: f64,
    /// Per-frame release coefficient for the sample rate in use.
    release: f64,
}

impl PeakLimiter {
    /// Creates a limiter at unity gain for the sample rate `fs` in Hz.
    #[must_use]
    pub fn new(fs: f64) -> Self {
        Self {
            gain: 1.0,
            release: release_coeff(fs),
        }
    }

    /// Updates the release time for a new sample rate. The gain state is kept.
    pub fn set_sample_rate(&mut self, fs: f64) {
        self.release = release_coeff(fs);
    }

    /// Returns the limiter to unity gain, forgetting any gain reduction.
    pub fn reset(&mut self) {
        self.gain = 1.0;
    }

    /// Current gain: 1.0 when no limiting is active.
    #[must_use]
    pub const fn gain(&self) -> f64 {
        self.gain
    }

    /// Limits one stereo frame `[left, right]` and returns the result.
    ///
    /// When the gain is exactly unity the input is returned unchanged, bit for bit.
    #[inline]
    pub fn process(&mut self, x: [f64; 2]) -> [f64; 2] {
        let peak = peak_abs(x[0], x[1]);
        let required = if peak > CEILING { CEILING / peak } else { 1.0 };

        if required < self.gain {
            // Instant attack.
            self.gain = required;
        } else {
            // Release towards unity.
            self.gain += (1.0 - self.gain) * self.release;
        }
        // Never exceed the gain this frame needs, even after the release step.
        self.gain = self.gain.min(required);

        if self.gain >= 1.0 {
            return x;
        }
        [x[0] * self.gain, x[1] * self.gain]
    }
}

/// Larger of the absolute values of two samples.
#[inline]
fn peak_abs(a: f64, b: f64) -> f64 {
    let a = if a < 0.0 { -a } else { a };
    let b = if b < 0.0 { -b } else { b };
    if a > b { a } else { b }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    /// A stereo sine at `amp` and `f0` Hz, sample `n` at `fs`.
    fn sine(n: usize, amp: f64, f0: f64, fs: f64) -> [f64; 2] {
        #[allow(clippy::cast_precision_loss, reason = "test sample indices are small")]
        let t = n as f64 / fs;
        let v = amp * libm::sin(2.0 * core::f64::consts::PI * f0 * t);
        [v, -v * 0.5]
    }

    #[test]
    fn ceiling_is_minus_0_3_dbfs() {
        assert_abs_diff_eq!(CEILING, 10_f64.powf(-0.3 / 20.0), epsilon = 1e-12);
    }

    #[test]
    fn loud_input_never_exceeds_ceiling() {
        let fs = 48_000.0;
        // +12 dBFS sine: far above the ceiling.
        let amp = 10_f64.powf(12.0 / 20.0);
        let mut limiter = PeakLimiter::new(fs);
        for n in 0..9_600 {
            let y = limiter.process(sine(n, amp, 1_000.0, fs));
            assert!(
                y[0].abs() <= CEILING + 1e-12 && y[1].abs() <= CEILING + 1e-12,
                "frame {n}: {y:?} exceeds the ceiling"
            );
        }
    }

    #[test]
    fn quiet_input_is_unchanged_bit_exact() {
        let fs = 48_000.0;
        let mut limiter = PeakLimiter::new(fs);
        for n in 0..4_800 {
            let x = sine(n, 0.5, 1_000.0, fs); // -6 dBFS
            assert_eq!(limiter.process(x), x);
        }
        assert_eq!(limiter.gain(), 1.0);
    }

    #[test]
    fn peak_reduces_gain_instantly_to_ceiling() {
        let mut limiter = PeakLimiter::new(48_000.0);
        let y = limiter.process([2.0, 1.0]);
        assert_abs_diff_eq!(y[0], CEILING, epsilon = 1e-12);
        assert_abs_diff_eq!(limiter.gain(), CEILING / 2.0, epsilon = 1e-12);
    }

    #[test]
    fn release_recovers_about_63_percent_in_100_ms() {
        let fs = 48_000.0;
        let mut limiter = PeakLimiter::new(fs);
        let _ = limiter.process([2.0, 0.0]);
        let g0 = limiter.gain();
        // 0.1 s at 48 kHz.
        for _ in 0..4_800 {
            let _ = limiter.process([0.0, 0.0]);
        }
        // After one time constant the remaining gap is exp(-1) of the initial gap.
        let expected = 1.0 - (1.0 - g0) * (-1.0_f64).exp();
        assert_abs_diff_eq!(limiter.gain(), expected, epsilon = 1e-3);
        assert!(limiter.gain() <= 1.0);
    }

    #[test]
    fn reset_restores_unity_gain() {
        let mut limiter = PeakLimiter::new(48_000.0);
        let _ = limiter.process([4.0, 4.0]);
        assert!(limiter.gain() < 1.0);
        limiter.reset();
        assert_eq!(limiter.gain(), 1.0);
    }

    #[test]
    fn gain_never_exceeds_unity() {
        let mut limiter = PeakLimiter::new(48_000.0);
        let _ = limiter.process([3.0, 0.0]);
        for _ in 0..10_000 {
            let _ = limiter.process([0.1, 0.1]);
            assert!(limiter.gain() <= 1.0);
        }
    }
}
