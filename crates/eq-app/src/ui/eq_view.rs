//! Geometry of the EQ widget: the response curve as SVG path commands and the band positions on
//! the logarithmic frequency axis.
//!
//! Everything here is pure (no Slint types), so the maths is covered by unit tests.

use std::fmt::Write as _;

use eq_dsp::{Coeffs, Layout, MAX_BANDS, corrected_gains, log_frequency, magnitude_db, peaking};

/// Sample rate used for the display curve. The UI only draws the curve, so it does not need
/// the device rate; the difference is far below what is visible.
const CURVE_SAMPLE_RATE: f64 = 48_000.0;

/// Number of points sampled along the response curve.
pub const CURVE_POINTS: usize = 128;

/// Largest gain, in dB, that still counts as flat. It is the precision the UI displays.
const FLAT_TOLERANCE_DB: f32 = 0.05;

/// Lowest frequency of the axis in Hz: the centre of the first band (32 Hz).
///
/// The axis runs from the first slider to the last one, so nothing is drawn beyond them.
const AXIS_MIN_HZ: f64 = Layout::Ten.centres()[0];

/// Highest frequency of the axis in Hz: the centre of the last band (16 kHz).
const AXIS_MAX_HZ: f64 = Layout::Ten.centres()[Layout::Ten.len() - 1];

/// Position of `hz` on the logarithmic axis: `0.0` at 32 Hz, `1.0` at 16 kHz.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "display geometry; f32 precision is far beyond the widget's pixel size"
)]
pub fn frequency_fraction(hz: f64) -> f32 {
    let t = (hz / AXIS_MIN_HZ).log10() / (AXIS_MAX_HZ / AXIS_MIN_HZ).log10();
    t as f32
}

/// Axis positions of all bands of `layout`, in the order of [`Layout::centres`].
///
/// The slider column of each band is placed at its fraction so that the sliders sit exactly
/// on the curve's frequency axis.
#[must_use]
pub fn band_positions(layout: Layout) -> Vec<f32> {
    layout
        .centres()
        .iter()
        .map(|&hz| frequency_fraction(hz))
        .collect()
}

/// Samples the response of the bands as `(x, y)` points in a `width` x `height` box.
///
/// `gains` are the slider values. The response is drawn from the corrected gains of accurate
/// mode, so the curve passes through each slider at its band centre.
///
/// The samples are log-spaced from the first band to the last one. X is linear in the sample
/// index, so point 0 is at the left edge (32 Hz) and the last point at the right edge (16 kHz).
/// Y is linear in dB: `+db_range` maps to the top edge, `-db_range` to the bottom edge and
/// 0 dB to the middle. Values beyond the range are clamped to the box.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "sample counts are tiny; the f32 geometry is for drawing only"
)]
pub fn curve_points(gains: &[f32], width: f32, height: f32, db_range: f32) -> Vec<(f32, f32)> {
    let layout = Layout::Ten;
    let mut corrected = [0.0_f32; MAX_BANDS];
    corrected_gains(layout, CURVE_SAMPLE_RATE, gains, &mut corrected);
    let coeffs: Vec<Coeffs> = layout
        .centres()
        .iter()
        .zip(&corrected)
        .map(|(&f0, &gain)| peaking(CURVE_SAMPLE_RATE, f0, layout.q(), f64::from(gain)))
        .collect();

    let half = height / 2.0;
    let scale = half / db_range.max(f32::EPSILON);
    let last = (CURVE_POINTS - 1) as f32;
    (0..CURVE_POINTS)
        .map(|i| {
            let hz = log_frequency(i, CURVE_POINTS, AXIS_MIN_HZ, AXIS_MAX_HZ);
            let db: f64 = coeffs
                .iter()
                .map(|c| magnitude_db(c, CURVE_SAMPLE_RATE, hz))
                .sum();
            // The curve is for display; f32 is ample for dB values.
            let value = db as f32;
            let x = width * i as f32 / last;
            let y = (half - value * scale).clamp(0.0, height);
            (x, y)
        })
        .collect()
}

/// SVG path of the response curve as an open polyline (no fill, for the stroke).
#[must_use]
pub fn curve_line_path(gains: &[f32], width: f32, height: f32, db_range: f32) -> String {
    let mut out = String::new();
    for (i, &(x, y)) in curve_points(gains, width, height, db_range)
        .iter()
        .enumerate()
    {
        push_command(&mut out, if i == 0 { 'M' } else { 'L' }, x, y);
    }
    out
}

/// `true` when every gain is within the display precision of 0 dB, so the curve lies on the
/// 0 dB line.
#[must_use]
pub fn is_flat(gains: &[f32]) -> bool {
    gains.iter().all(|gain| gain.abs() < FLAT_TOLERANCE_DB)
}

/// SVG path of the filled area between the response curve and the 0 dB line.
///
/// The shape is the curve followed by the baseline back to the left edge, closed with `Z`.
/// A flat curve encloses no area, so its fill is an empty path: the shape would be degenerate
/// and the renderer then draws it as a stray half-coloured line next to the stroke.
#[must_use]
pub fn curve_path(gains: &[f32], width: f32, height: f32, db_range: f32) -> String {
    if is_flat(gains) {
        return String::new();
    }
    let mut out = curve_line_path(gains, width, height, db_range);
    let mid = height / 2.0;
    push_command(&mut out, 'L', width, mid);
    push_command(&mut out, 'L', 0.0, mid);
    out.push_str(" Z");
    out
}

/// Appends one absolute path command (`M` or `L`) with its coordinates.
fn push_command(out: &mut String, command: char, x: f32, y: f32) {
    if !out.is_empty() {
        out.push(' ');
    }
    // Writing into a String cannot fail.
    let _ = write!(out, "{command} {x:.2} {y:.2}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use eq_dsp::log_frequency;

    const WIDTH: f32 = 1000.0;
    const HEIGHT: f32 = 200.0;
    const RANGE: f32 = 12.0;

    #[test]
    fn flat_gains_give_a_straight_line_at_mid_height() {
        let points = curve_points(&[0.0; 10], WIDTH, HEIGHT, RANGE);
        assert_eq!(points.len(), CURVE_POINTS);
        for (i, &(x, y)) in points.iter().enumerate() {
            assert!((y - HEIGHT / 2.0).abs() < 1e-3, "point {i} at y={y}");
            assert!(
                (0.0..=WIDTH).contains(&x),
                "point {i} outside the box at x={x}"
            );
        }
        assert!(points[0].0.abs() < 1e-3);
        assert!((points[CURVE_POINTS - 1].0 - WIDTH).abs() < 1e-3);
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "test arithmetic on a small sample count"
    )]
    fn plus_twelve_db_at_1khz_peaks_near_1khz() {
        // Index 5 of the 10-band layout is the 1 kHz band.
        let mut gains = [0.0_f32; 10];
        gains[5] = 12.0;
        let points = curve_points(&gains, WIDTH, HEIGHT, RANGE);

        let (peak_index, &(peak_x, peak_y)) = points
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
            .expect("curve has points");

        // The highest point is the top of the box (+12 dB) and sits at the 1 kHz x position,
        // within one sample step of the logarithmic grid.
        assert!(peak_y.abs() < 0.5, "peak y={peak_y}");
        let expected_x = WIDTH * frequency_fraction(1_000.0);
        let step = WIDTH / (CURVE_POINTS - 1) as f32;
        assert!(
            (peak_x - expected_x).abs() <= step,
            "peak at index {peak_index} (x={peak_x}), expected x={expected_x}"
        );
        let peak_hz = log_frequency(peak_index, CURVE_POINTS, AXIS_MIN_HZ, AXIS_MAX_HZ);
        assert!(
            (peak_hz / 1_000.0 - 1.0).abs() < 0.08,
            "peak at {peak_hz} Hz"
        );
    }

    #[test]
    fn band_positions_span_the_axis_exactly() {
        let positions = band_positions(Layout::Ten);
        assert_eq!(positions.len(), 10);
        for pair in positions.windows(2) {
            assert!(pair[0] < pair[1]);
        }
        // The first and last sliders sit on the edges of the plot, not beyond them.
        assert_eq!(positions[0], 0.0);
        assert_eq!(positions[9], 1.0);
        assert_eq!(frequency_fraction(32.0), 0.0);
        assert_eq!(frequency_fraction(16_000.0), 1.0);
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "test arithmetic on a small sample count"
    )]
    fn curve_follows_the_slider_axis_when_scaled() {
        // The curve is drawn in the same box as the sliders' axis: a point at 1 kHz must line
        // up with the 1 kHz band for any plot width.
        for width in [400.0_f32, 1_000.0, 1_600.0] {
            let fraction = frequency_fraction(1_000.0);
            let mut gains = [0.0_f32; 10];
            gains[5] = 12.0;
            let points = curve_points(&gains, width, HEIGHT, RANGE);
            let (_, &(peak_x, _)) = points
                .iter()
                .enumerate()
                .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
                .expect("curve has points");
            let step = width / (CURVE_POINTS - 1) as f32;
            assert!((peak_x - width * fraction).abs() <= step);
        }
    }

    #[test]
    fn plus_ten_db_at_32hz_peaks_at_the_left_edge() {
        let mut gains = [0.0_f32; 10];
        gains[0] = 10.0;
        let points = curve_points(&gains, WIDTH, HEIGHT, RANGE);
        let (peak_index, &(peak_x, _)) = points
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.1.total_cmp(&b.1.1))
            .expect("curve has points");
        assert_eq!(peak_index, 0);
        assert!(peak_x.abs() < 1e-3);
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "test arithmetic on a small sample count"
    )]
    fn curve_passes_through_the_handles_of_overlapping_bands() {
        // +10 dB at 32 Hz next to -12 dB at 63 Hz: without correction the curve would sit near
        // +7.5 dB at 32 Hz.
        let mut gains = [0.0_f32; 10];
        gains[0] = 10.0;
        gains[1] = -12.0;
        let points = curve_points(&gains, WIDTH, HEIGHT, RANGE);
        let half = HEIGHT / 2.0;
        let db_at = |i: usize| f64::from((half - points[i].1) * RANGE / half);

        // Point 0 is the 32 Hz band.
        assert!((db_at(0) - 10.0).abs() <= 0.5, "32 Hz: {} dB", db_at(0));

        // 63 Hz falls between two samples: interpolate in the sample index.
        let pos = frequency_fraction(63.0) * (CURVE_POINTS - 1) as f32;
        let i = pos.floor() as usize;
        let t = f64::from(pos) - i as f64;
        let db = db_at(i) * (1.0 - t) + db_at(i + 1) * t;
        assert!((db + 12.0).abs() <= 0.5, "63 Hz: {db} dB");
    }

    #[test]
    fn curve_ends_are_at_the_first_and_last_band() {
        let mut gains = [0.0_f32; 10];
        gains[9] = 4.0;
        let points = curve_points(&gains, WIDTH, HEIGHT, RANGE);
        assert!(points[0].0.abs() < 1e-3);
        assert!((points[CURVE_POINTS - 1].0 - WIDTH).abs() < 1e-3);
    }

    #[test]
    fn filled_path_is_closed_down_to_the_zero_db_line() {
        let mut gains = [0.0_f32; 10];
        gains[0] = 6.0;
        let path = curve_path(&gains, WIDTH, HEIGHT, RANGE);
        assert!(path.starts_with("M 0.00 "));
        assert!(path.ends_with(" Z"));
        // The curve ends at the right edge, then the baseline closes it back to the left edge.
        assert!(path.contains(" L 1000.00 100.00 L 0.00 100.00 Z"));
        assert_eq!(path.matches('M').count(), 1);
    }

    #[test]
    fn flat_curve_has_no_fill_but_keeps_its_line() {
        assert!(is_flat(&[0.0; 10]));
        assert!(is_flat(&[
            0.04, -0.04, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0
        ]));
        assert_eq!(curve_path(&[0.0; 10], WIDTH, HEIGHT, RANGE), "");
        assert_ne!(curve_line_path(&[0.0; 10], WIDTH, HEIGHT, RANGE), "");
    }

    #[test]
    fn non_flat_curve_has_fill_and_line() {
        let mut gains = [0.0_f32; 10];
        gains[9] = -3.0;
        assert!(!is_flat(&gains));
        assert_ne!(curve_path(&gains, WIDTH, HEIGHT, RANGE), "");
        assert_ne!(curve_line_path(&gains, WIDTH, HEIGHT, RANGE), "");
    }

    #[test]
    fn line_path_is_open() {
        let path = curve_line_path(&[0.0; 10], WIDTH, HEIGHT, RANGE);
        assert!(path.starts_with("M "));
        assert!(!path.contains('Z'));
        assert_eq!(path.matches('M').count(), 1);
        assert_eq!(path.matches('L').count(), CURVE_POINTS - 1);
    }

    #[test]
    fn out_of_range_gains_are_clamped_to_the_box() {
        let points = curve_points(&[12.0; 10], WIDTH, HEIGHT, RANGE);
        for &(_, y) in &points {
            assert!((0.0..=HEIGHT).contains(&y));
        }
    }
}
