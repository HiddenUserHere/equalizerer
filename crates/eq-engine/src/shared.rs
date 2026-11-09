//! Handoff of settings and statistics between the controller thread and the audio thread.
//!
//! - Parameters travel through a triple buffer: the writer never blocks, and the reader always
//!   gets the most recent value without allocating or locking.
//! - Statistics are plain atomics shared through an `Arc` owned by the caller.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use eq_dsp::EqParams;

/// Statistics reported by a running pipeline, as a plain value.
///
/// This mirrors the fields of `eq_core::Stats`. `eq-engine` does not depend on `eq-core`, so
/// the backends convert between the two.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EngineStats {
    /// Measured end-to-end latency in milliseconds.
    pub latency_ms: f32,
    /// Number of buffer underruns since the pipeline started.
    pub underruns: u64,
    /// Number of drift resyncs since the pipeline started.
    pub resyncs: u64,
}

/// Controller-side end of the parameter handoff. Created by [`params_channel`].
#[derive(Debug)]
pub struct ParamsWriter {
    input: triple_buffer::Input<EqParams>,
}

impl ParamsWriter {
    /// Publishes new settings. Never blocks; the previous unread value is overwritten.
    pub fn publish(&mut self, params: EqParams) {
        self.input.write(params);
    }
}

/// Audio-side end of the parameter handoff. Created by [`params_channel`].
///
/// Both methods are allocation-free and lock-free, so they may be called on the audio thread.
/// The processor should call [`latest`](Self::latest) once per period.
#[derive(Debug)]
pub struct ParamsReader {
    output: triple_buffer::Output<EqParams>,
}

impl ParamsReader {
    /// Fetches the most recently published settings and returns a reference to them.
    ///
    /// Repeated calls without a new publish return the same value.
    pub fn latest(&mut self) -> &EqParams {
        self.output.read()
    }

    /// Returns `true` if a value was published since the last call to [`latest`](Self::latest).
    ///
    /// This is a hint, not a synchronisation point: the flag is only updated by the writer,
    /// so a `true` result may be stale by the time it is used. Use it to skip work such as
    /// recomputing filter coefficients, and always go through [`latest`](Self::latest) for
    /// the values themselves.
    #[must_use]
    pub fn has_update(&self) -> bool {
        self.output.updated()
    }
}

/// Creates a connected parameter writer and reader, both starting from `initial`.
///
/// The triple buffer is allocated here. Neither half allocates afterwards.
#[must_use]
pub fn params_channel(initial: EqParams) -> (ParamsWriter, ParamsReader) {
    let (input, output) = triple_buffer::triple_buffer(&initial);
    (ParamsWriter { input }, ParamsReader { output })
}

/// Live statistics shared between the audio thread (writer) and the controller (reader).
///
/// Share it through an `Arc`. Every method takes `&self`, so the struct itself holds only
/// atomics.
///
/// All accesses use [`Ordering::Relaxed`]. Each value is independent of the others and no
/// other memory is published through these atomics, so no acquire/release pairing is needed.
/// The counters are monotonic. A [`snapshot`](Self::snapshot) reads the fields one by one, so
/// it can mix values from slightly different moments; that is acceptable for display.
#[derive(Debug, Default)]
pub struct SharedStats {
    /// `f32` latency in milliseconds, stored as its IEEE-754 bit pattern.
    latency_ms_bits: AtomicU32,
    underruns: AtomicU64,
    resyncs: AtomicU64,
}

impl SharedStats {
    /// Creates zeroed statistics.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores the latest measured latency in milliseconds.
    pub fn set_latency_ms(&self, latency_ms: f32) {
        self.latency_ms_bits
            .store(latency_ms.to_bits(), Ordering::Relaxed);
    }

    /// Counts one buffer underrun.
    pub fn add_underrun(&self) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
    }

    /// Counts one drift resync.
    pub fn add_resync(&self) {
        self.resyncs.fetch_add(1, Ordering::Relaxed);
    }

    /// Reads all statistics into a plain [`EngineStats`] value.
    #[must_use]
    pub fn snapshot(&self) -> EngineStats {
        EngineStats {
            latency_ms: f32::from_bits(self.latency_ms_bits.load(Ordering::Relaxed)),
            underruns: self.underruns.load(Ordering::Relaxed),
            resyncs: self.resyncs.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn reader_starts_with_initial_value() {
        let (_writer, mut reader) = params_channel(EqParams::default());
        assert!(!reader.has_update());
        assert_eq!(*reader.latest(), EqParams::default());
    }

    #[test]
    fn reader_sees_latest_publish() {
        let (mut writer, mut reader) = params_channel(EqParams::default());
        let mut changed = EqParams::default();
        changed.gains[3] = 6.0;
        writer.publish(changed);
        assert!(reader.has_update());
        assert_eq!(reader.latest().gains[3], 6.0);
        assert!(!reader.has_update());
    }

    #[test]
    #[allow(
        clippy::cast_precision_loss,
        reason = "values up to 10 000 are exact in f32"
    )]
    fn writer_thread_publishes_and_reader_eventually_sees_last() {
        const LAST: u32 = 10_000;
        let (mut writer, mut reader) = params_channel(EqParams::default());

        let publisher = std::thread::spawn(move || {
            for i in 1..=LAST {
                let mut params = EqParams::default();
                params.gains[0] = i as f32;
                writer.publish(params);
            }
        });

        // The triple buffer only ever hands out the newest published value, so the observed
        // sequence must be non-decreasing and must end at LAST.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_seen = 0.0_f32;
        while last_seen < LAST as f32 {
            assert!(Instant::now() < deadline, "reader never saw the last value");
            let seen = reader.latest().gains[0];
            assert!(
                seen >= last_seen,
                "values went backwards: {seen} < {last_seen}"
            );
            last_seen = seen;
        }
        publisher.join().expect("publisher thread panicked");
        assert_eq!(reader.latest().gains[0], LAST as f32);
    }

    #[test]
    fn stats_snapshot_reflects_updates() {
        let stats = SharedStats::new();
        assert_eq!(stats.snapshot(), EngineStats::default());

        stats.set_latency_ms(12.5);
        stats.add_underrun();
        stats.add_underrun();
        stats.add_underrun();
        stats.add_resync();
        assert_eq!(
            stats.snapshot(),
            EngineStats {
                latency_ms: 12.5,
                underruns: 3,
                resyncs: 1,
            }
        );

        stats.set_latency_ms(-0.0);
        assert_eq!(stats.snapshot().latency_ms.to_bits(), (-0.0_f32).to_bits());
    }

    #[test]
    fn stats_are_shared_across_threads() {
        let stats = Arc::new(SharedStats::new());
        let writer = {
            let stats = Arc::clone(&stats);
            std::thread::spawn(move || {
                for _ in 0..1_000 {
                    stats.add_underrun();
                    stats.add_resync();
                }
                stats.set_latency_ms(42.0);
            })
        };
        writer.join().expect("writer thread panicked");
        let snapshot = stats.snapshot();
        assert_eq!(snapshot.underruns, 1_000);
        assert_eq!(snapshot.resyncs, 1_000);
        assert_eq!(snapshot.latency_ms, 42.0);
    }
}
