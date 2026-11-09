//! Second-order IIR section (biquad) in transposed direct form II.
//!
//! Coefficients and state are kept in `f64`; the stereo pair is stored as one `[f64; 2]`
//! lane per state variable so the per-sample loop auto-vectorises.

/// Normalised biquad coefficients (`a0` is folded into the others, so it is implicitly 1).
///
/// The transfer function is `(b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    /// Feed-forward coefficient for the current sample.
    pub b0: f64,
    /// Feed-forward coefficient for the previous sample.
    pub b1: f64,
    /// Feed-forward coefficient for the sample two steps back.
    pub b2: f64,
    /// Feedback coefficient for the previous output.
    pub a1: f64,
    /// Feedback coefficient for the output two steps back.
    pub a2: f64,
}

impl Coeffs {
    /// Coefficients of a pass-through filter: the output equals the input.
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };
}

/// Stereo biquad: one filter shared by the left and right channels with separate state.
///
/// Transposed direct form II, per sample and per channel:
/// `y = b0*x + s1; s1 = b1*x - a1*y + s2; s2 = b2*x - a2*y`.
/// Transposed form keeps the rounding error of the state low at `f64` precision and needs
/// only two state registers per channel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StereoBiquad {
    /// Coefficients currently in use.
    pub coeffs: Coeffs,
    /// First state variable, `[left, right]`.
    s1: [f64; 2],
    /// Second state variable, `[left, right]`.
    s2: [f64; 2],
}

impl StereoBiquad {
    /// Creates a filter with the given coefficients and zeroed state.
    #[must_use]
    pub const fn new(coeffs: Coeffs) -> Self {
        Self {
            coeffs,
            s1: [0.0; 2],
            s2: [0.0; 2],
        }
    }

    /// Replaces the coefficients without touching the state.
    ///
    /// The state is kept so that a coefficient change does not reset the filter memory,
    /// which would produce an audible click.
    pub fn set_coeffs(&mut self, coeffs: Coeffs) {
        self.coeffs = coeffs;
    }

    /// Clears the filter memory, leaving the coefficients unchanged.
    pub fn reset(&mut self) {
        self.s1 = [0.0; 2];
        self.s2 = [0.0; 2];
    }

    /// Filters one stereo frame `[left, right]` and returns the filtered frame.
    #[inline]
    pub fn process(&mut self, x: [f64; 2]) -> [f64; 2] {
        let c = self.coeffs;
        let mut y = [0.0; 2];
        for (((out, &xi), s1), s2) in y.iter_mut().zip(&x).zip(&mut self.s1).zip(&mut self.s2) {
            *out = c.b0 * xi + *s1;
            *s1 = c.b1 * xi - c.a1 * *out + *s2;
            *s2 = c.b2 * xi - c.a2 * *out;
        }
        y
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    #[test]
    fn identity_is_passthrough() {
        let mut f = StereoBiquad::new(Coeffs::IDENTITY);
        for &x in &[[0.5, -0.25], [1.0, 1.0], [-0.75, 0.125], [0.0, 0.3]] {
            let y = f.process(x);
            assert_abs_diff_eq!(y[0], x[0], epsilon = 1e-12);
            assert_abs_diff_eq!(y[1], x[1], epsilon = 1e-12);
        }
    }

    #[test]
    fn impulse_response_matches_hand_computed_values() {
        // b = [1, 0.5, 0.25], a = [1, -0.5, 0.125]. Recurrence by hand:
        // y[n] = b0 x[n] + b1 x[n-1] + b2 x[n-2] - a1 y[n-1] - a2 y[n-2]
        // y0 = 1, y1 = 1.0, y2 = 0.625, y3 = 0.1875, y4 = 0.015625, y5 = -0.015625.
        let expected = [1.0, 1.0, 0.625, 0.1875, 0.015_625, -0.015_625];
        let mut f = StereoBiquad::new(Coeffs {
            b0: 1.0,
            b1: 0.5,
            b2: 0.25,
            a1: -0.5,
            a2: 0.125,
        });
        for (n, &want) in expected.iter().enumerate() {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let y = f.process([x, x]);
            assert_abs_diff_eq!(y[0], want, epsilon = 1e-12);
            assert_abs_diff_eq!(y[1], want, epsilon = 1e-12);
        }
    }

    #[test]
    fn channels_have_independent_state() {
        let mut f = StereoBiquad::new(Coeffs {
            b0: 1.0,
            b1: 0.5,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        });
        let y0 = f.process([1.0, 0.0]);
        let y1 = f.process([0.0, 0.0]);
        assert_abs_diff_eq!(y0[0], 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(y0[1], 0.0, epsilon = 1e-12);
        assert_abs_diff_eq!(y1[0], 0.5, epsilon = 1e-12);
        assert_abs_diff_eq!(y1[1], 0.0, epsilon = 1e-12);
    }

    #[test]
    fn reset_zeroes_state() {
        let mut f = StereoBiquad::new(Coeffs {
            b0: 1.0,
            b1: 0.5,
            b2: 0.25,
            a1: -0.5,
            a2: 0.125,
        });
        let _ = f.process([1.0, -1.0]);
        let _ = f.process([0.5, 0.5]);
        f.reset();
        assert_eq!(f.s1, [0.0; 2]);
        assert_eq!(f.s2, [0.0; 2]);
        // After a reset the response to an impulse starts from scratch again.
        let y = f.process([1.0, 1.0]);
        assert_abs_diff_eq!(y[0], 1.0, epsilon = 1e-12);
        assert_abs_diff_eq!(y[1], 1.0, epsilon = 1e-12);
    }

    #[test]
    fn set_coeffs_keeps_state() {
        let mut f = StereoBiquad::new(Coeffs::IDENTITY);
        f.s1 = [0.3, 0.4];
        f.set_coeffs(Coeffs {
            b0: 2.0,
            ..Coeffs::IDENTITY
        });
        assert_eq!(f.s1, [0.3, 0.4]);
        assert_abs_diff_eq!(f.coeffs.b0, 2.0, epsilon = 1e-12);
    }
}
