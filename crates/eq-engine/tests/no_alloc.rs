//! Verifies that the FIFO hot paths never allocate after construction.
//!
//! `assert_no_alloc` installs a global allocator that aborts (or warns) when an allocation
//! happens inside `assert_no_alloc(..)` on the current thread.

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use eq_engine::{FrameSource, StereoFifo, split_spsc};

#[global_allocator]
static A: AllocDisabler = AllocDisabler;

#[test]
fn stereo_fifo_hot_paths_do_not_allocate() {
    // Construction allocates; everything after it must not.
    let mut fifo = StereoFifo::with_capacity_frames(64);
    let input = [0.5_f32; 2 * 48];
    let mut out = [0.0_f32; 2 * 48];

    assert_no_alloc(|| {
        assert_eq!(fifo.push(&input), 48);
        assert_eq!(fifo.pad_silence(10), 10);
        assert_eq!(fifo.len_frames(), 58);
        assert_eq!(FrameSource::pop_into(&mut fifo, &mut out), 48);
        fifo.discard(5);
        assert_eq!(fifo.push(&input), 48);
        fifo.clear();
    });
}

#[test]
fn spsc_hot_paths_do_not_allocate() {
    let (mut producer, mut consumer) = split_spsc(64);
    let input = [0.25_f32; 2 * 32];
    let mut out = [0.0_f32; 2 * 32];

    assert_no_alloc(|| {
        assert_eq!(producer.push(&input), 32);
        assert_eq!(consumer.len_frames(), 32);
        assert_eq!(FrameSource::pop_into(&mut consumer, &mut out), 32);
        assert_eq!(producer.push(&input), 32);
        consumer.discard(16);
        assert_eq!(consumer.pop_into(&mut out), 16);
    });
}

#[test]
fn processor_render_does_not_allocate() {
    use std::sync::Arc;

    use eq_dsp::EqParams;
    use eq_engine::{Processor, ProcessorConfig, SharedStats, params_channel};

    const PERIOD: usize = 480;
    let (mut writer, reader) = params_channel(EqParams::default());
    let stats = Arc::new(SharedStats::new());
    let config = ProcessorConfig {
        sample_rate: 48_000.0,
        out_channels: 6,
        fifo_target_frames: 1_440,
        max_block_frames: 256,
    };
    let mut processor = Processor::new(config, reader, Arc::clone(&stats)).unwrap();
    let mut src = StereoFifo::with_capacity_frames(16_384);
    let input = [0.25_f32; 2 * PERIOD];
    // Build the initial fill outside the checked region; construction allocates by design.
    assert_eq!(src.push(&input), PERIOD);
    assert_eq!(src.push(&input), PERIOD);
    assert_eq!(src.push(&input), PERIOD);
    let mut out = vec![0.0_f32; 6 * PERIOD];
    let mut large = vec![0.0_f32; 6 * 1_000];

    let mut boosted = EqParams::default();
    boosted.gains[5] = 6.0;

    assert_no_alloc(|| {
        // Normal periods, with a parameter change in the middle.
        for period in 0..6 {
            if period == 3 {
                writer.publish(boosted);
            }
            assert_eq!(src.push(&input), PERIOD);
            processor.render(&mut src, &mut out);
        }
        // Multi-block render request (larger than max_block_frames).
        assert_eq!(src.push(&input), PERIOD);
        processor.render(&mut src, &mut large);
        // Starved render: the source is empty, so the resync path primes silence.
        src.clear();
        processor.render(&mut src, &mut out);
    });
    assert!(stats.snapshot().underruns >= 1);
}
