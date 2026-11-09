//! The real-time paths must not allocate. Runs with `assert_no_alloc`, which aborts on any
//! heap allocation or deallocation made inside the wrapped closure.

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use eq_dsp::GraphicEq;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const FS: f64 = 48_000.0;

fn fill_sine(buf: &mut [f32], start_frame: usize) {
    for (i, frame) in buf.as_chunks_mut::<2>().0.iter_mut().enumerate() {
        let n = start_frame + i;
        #[allow(clippy::cast_precision_loss, reason = "test frame indices are small")]
        let t = n as f64 / FS;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "test signal is generated in f32 audio format"
        )]
        let v = (0.2 * (2.0 * core::f64::consts::PI * 1_000.0 * t).sin()) as f32;
        frame[0] = v;
        frame[1] = v;
    }
}

#[test]
fn process_interleaved_does_not_allocate_during_ramps_and_toggles() {
    let mut eq = GraphicEq::new(FS);
    let mut buf = vec![0.0_f32; 2 * 480];
    let gains_a = [3.0, -2.0, 4.0, -1.0, 2.0, -3.0, 1.0, -4.0, 2.0, 3.0];
    let gains_b = [-3.0, 2.0, -4.0, 1.0, -2.0, 3.0, -1.0, 4.0, -2.0, -3.0];

    for block in 0..12 {
        fill_sine(&mut buf, block * 480);
        assert_no_alloc(|| {
            // Alternate gains so ramps are always in progress, and toggle power every few blocks.
            // Accurate mode stays on, so each gain change also runs the band correction; it is
            // switched off and on every few blocks to re-apply the sliders.
            eq.set_gains(if block % 2 == 0 { &gains_a } else { &gains_b });
            eq.set_enabled(block % 5 != 4);
            eq.set_accurate(block % 3 != 2);
            eq.process_interleaved(&mut buf);
        });
    }
}
