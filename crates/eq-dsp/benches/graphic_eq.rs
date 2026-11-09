//! Criterion benchmarks for the graphic EQ: 480-frame stereo blocks at 48 kHz, all ten bands
//! at non-zero gain.

use criterion::{Criterion, criterion_group, criterion_main};
use eq_dsp::{GraphicEq, Layout, MAX_BANDS, corrected_gains};
use std::hint::black_box;

const FS: f64 = 48_000.0;
/// Frames per block, the period size used by the engine.
const FRAMES: usize = 480;
const GAINS_A: [f32; 10] = [3.0, -2.0, 4.0, -1.0, 2.0, -3.0, 1.0, -4.0, 2.0, 3.0];
const GAINS_B: [f32; 10] = [-3.0, 2.0, -4.0, 1.0, -2.0, 3.0, -1.0, 4.0, -2.0, -3.0];

/// A stereo test block of `FRAMES` frames with a 1 kHz sine at -20 dBFS.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "benchmark input; the exact signal does not matter, only that it is audio-like"
)]
fn test_block() -> Vec<f32> {
    let mut buf = vec![0.0_f32; 2 * FRAMES];
    for (n, frame) in buf.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let t = n as f64 / FS;
        let v = (0.1 * (2.0 * std::f64::consts::PI * 1_000.0 * t).sin()) as f32;
        frame[0] = v;
        frame[1] = v;
    }
    buf
}

fn bench_graphic_eq(c: &mut Criterion) {
    let mut group = c.benchmark_group("graphic_eq");

    group.bench_function("steady_state_10_bands", |b| {
        let mut eq = GraphicEq::new(FS);
        eq.set_gains(&GAINS_A);
        let mut buf = test_block();
        // Let all ramps finish before measuring.
        for _ in 0..8 {
            eq.process_interleaved(&mut buf);
        }
        b.iter(|| eq.process_interleaved(black_box(&mut buf)));
    });

    group.bench_function("ramping_10_bands", |b| {
        let mut eq = GraphicEq::new(FS);
        let mut buf = test_block();
        let mut toggle = false;
        // Each iteration sets new targets for all bands, so every band ramps during the block.
        // The timing includes the `set_gains` call, which runs the band correction (accurate
        // mode) and computes the new target coefficients.
        b.iter(|| {
            let gains = if toggle { &GAINS_A } else { &GAINS_B };
            toggle = !toggle;
            eq.set_gains(black_box(gains));
            eq.process_interleaved(black_box(&mut buf));
        });
    });

    group.finish();
}

/// Cost of the band-interaction correction alone, run for every gain change in accurate mode.
fn bench_correction(c: &mut Criterion) {
    let mut group = c.benchmark_group("correction");
    group.bench_function("corrected_gains_10_bands", |b| {
        let mut out = [0.0_f32; MAX_BANDS];
        b.iter(|| {
            corrected_gains(Layout::Ten, FS, black_box(&GAINS_A), &mut out);
            black_box(&out);
        });
    });
    group.finish();
}

criterion_group!(benches, bench_graphic_eq, bench_correction);
criterion_main!(benches);
