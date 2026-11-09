//! The audio processor: drift compensation, graphic EQ and up-mix, run once per render period.
//!
//! [`Processor::render`] is the shared hot path of the platform backends. It takes the captured
//! frames from a [`FrameSource`], pulls frames at the render clock through the
//! [`DriftResampler`], applies the [`GraphicEq`] and writes the requested number of output
//! channels. Parameters arrive through the triple buffer and statistics leave through atomics,
//! so the render call neither allocates nor locks.

use std::sync::Arc;

use eq_dsp::{EqParams, GraphicEq};
use thiserror::Error;

use crate::drift::{DriftAction, DriftConfig, DriftController, DriftResampler};
use crate::fifo::FrameSource;
use crate::shared::{ParamsReader, SharedStats};

/// Channels of the internal processing buffers (stereo).
const CHANNELS: usize = 2;

/// Static configuration of a [`Processor`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessorConfig {
    /// Render sample rate in Hz (the rate of the output device).
    pub sample_rate: f64,
    /// Number of interleaved channels in the output buffer. Values below 1 are treated as 1.
    pub out_channels: usize,
    /// Fill the drift controller steers the capture FIFO to, in frames (the latency target).
    pub fifo_target_frames: usize,
    /// Largest number of frames processed in one internal block. Longer render requests are
    /// split into blocks of this size. Values below 1 are treated as 1.
    pub max_block_frames: usize,
}

/// Errors from [`Processor::new`].
#[derive(Debug, Error)]
pub enum ProcessorError {
    /// The sample rate is zero, negative, NaN or infinite.
    #[error("sample rate must be a positive, finite number, got {0}")]
    InvalidSampleRate(f64),
    /// The resampler could not be created with the given configuration.
    #[error("failed to create the drift resampler: {0}")]
    Resampler(#[from] rubato::ResamplerConstructionError),
}

/// Real-time processor that turns captured frames into a rendered output buffer.
///
/// Create it on the controller side with [`Processor::new`] (this allocates) and move it to the
/// audio thread. Everything else, including [`Processor::render`], is allocation-free.
///
/// Latency reporting: after every render call the processor publishes the buffered audio in
/// milliseconds: frames waiting in the capture FIFO plus frames staged in the resampler, divided
/// by the sample rate, plus the device latency that the backend reports through
/// [`Processor::set_device_latency_ms`]. The resampler's own filter delay (a few frames) and the
/// latency inside the OS audio stacks are not included, except through the device value.
#[derive(Debug)]
pub struct Processor {
    /// Graphic equalizer running at the render sample rate.
    eq: GraphicEq,
    /// Triple-buffer reader for the controller's settings.
    params: ParamsReader,
    /// The settings currently applied to `eq`, used to skip redundant work.
    applied: EqParams,
    /// Statistics published to the controller.
    stats: Arc<SharedStats>,
    /// PI controller that steers the resampling ratio.
    drift: DriftController,
    /// Resampler with its staging FIFO.
    resampler: DriftResampler,
    /// Interleaved stereo block buffer, sized for `max_block_frames`.
    scratch: Vec<f32>,
    /// Render sample rate in Hz, used for the latency calculation.
    sample_rate: f64,
    /// Output channels per frame, at least 1.
    out_channels: usize,
    /// Largest internal block in frames, at least 1.
    max_block_frames: usize,
    /// Fill target in frames, at least 1.
    target_frames: usize,
    /// Device-side latency added to the published latency, in milliseconds.
    device_latency_ms: f32,
}

impl Processor {
    /// Creates a processor with all buffers allocated.
    ///
    /// The initial parameters are read from `params` and applied, so the first render already
    /// uses them. `stats` is shared with the controller; the processor only writes to it.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessorError::InvalidSampleRate`] for a non-positive or non-finite sample
    /// rate, and [`ProcessorError::Resampler`] if the resampler cannot be created.
    pub fn new(
        config: ProcessorConfig,
        params: ParamsReader,
        stats: Arc<SharedStats>,
    ) -> Result<Self, ProcessorError> {
        let fs = config.sample_rate;
        if !(fs.is_finite() && fs > 0.0) {
            return Err(ProcessorError::InvalidSampleRate(fs));
        }
        let max_block_frames = config.max_block_frames.max(1);
        let target_frames = config.fifo_target_frames.max(1);
        let drift_config = DriftConfig::new(fs, target_frames);
        let mut processor = Self {
            eq: GraphicEq::new(fs),
            params,
            applied: EqParams::default(),
            stats,
            drift: DriftController::new(&drift_config),
            resampler: DriftResampler::new(&drift_config, max_block_frames.max(target_frames))?,
            scratch: vec![0.0; max_block_frames * CHANNELS],
            sample_rate: fs,
            out_channels: config.out_channels.max(1),
            max_block_frames,
            target_frames,
            device_latency_ms: 0.0,
        };
        processor.apply_params();
        Ok(processor)
    }

    /// Sets the device-side latency that is added to the published latency.
    ///
    /// The backend calls this when it learns the device latency (for example from
    /// `GetStreamLatency` or the `PipeWire` stream timing). NaN and negative values count as zero.
    pub fn set_device_latency_ms(&mut self, latency_ms: f32) {
        self.device_latency_ms = if latency_ms.is_finite() && latency_ms > 0.0 {
            latency_ms
        } else {
            0.0
        };
    }

    /// Renders `out.len() / out_channels` frames from `src` into `out`.
    ///
    /// Steps per call: read the parameters once, then for each block of at most
    /// `max_block_frames` frames update the drift controller, resync if the fill is far off,
    /// pull resampled frames, apply the EQ and up-mix into `out`. A source that cannot supply
    /// the frames yields zeros for the missing part and counts one underrun per call. A call
    /// that finds the buffers completely empty also resyncs by priming silence up to the target.
    ///
    /// Samples after the last whole frame of `out` are set to zero.
    pub fn render(&mut self, src: &mut impl FrameSource, out: &mut [f32]) {
        self.apply_params();
        let channels = self.out_channels;
        let total = out.len() / channels;
        let mut underrun = false;
        let mut offset = 0;
        while offset < total {
            let frames = (total - offset).min(self.max_block_frames);
            underrun |= self.render_block(src, frames);
            let block = &mut out[offset * channels..(offset + frames) * channels];
            self.write_output(block);
            offset += frames;
        }
        out[total * channels..].fill(0.0);
        if underrun {
            self.stats.add_underrun();
        }
        self.publish_latency(src.len_frames());
    }

    /// Applies the controller's settings to the equalizer if they changed since the last call.
    fn apply_params(&mut self) {
        let params = *self.params.latest();
        if params == self.applied {
            return;
        }
        if params.layout != self.applied.layout {
            self.eq.set_layout(params.layout);
        }
        if params.layout != self.applied.layout || !same_gains(&params.gains, &self.applied.gains) {
            self.eq.set_gains(params.band_gains());
        }
        if params.enabled != self.applied.enabled {
            self.eq.set_enabled(params.enabled);
        }
        self.applied = params;
    }

    /// Produces `frames` processed stereo frames into the scratch buffer.
    ///
    /// Returns `true` if the source ran out of frames during this block.
    fn render_block(&mut self, src: &mut impl FrameSource, frames: usize) -> bool {
        let fill = src.len_frames() + self.resampler.staged_frames();
        let mut ratio = self.drift.update(fill, frames);
        if let DriftAction::ResyncTo(_) = self.drift.action(fill) {
            self.resync_to_target(src);
            ratio = 1.0;
        }

        let fill_result = self.resampler.fill_output(src, ratio, frames);
        let samples = &mut self.scratch[..frames * CHANNELS];
        let got = self.resampler.pop_output(samples);
        samples[got * CHANNELS..].fill(0.0);

        if fill_result.starved && src.len_frames() == 0 && self.resampler.staged_frames() == 0 {
            // Nothing is buffered any more: prime silence up to the target instead of
            // underrunning on every request until the source catches up.
            self.resync_to_target(src);
        }

        self.eq
            .process_interleaved(&mut self.scratch[..frames * CHANNELS]);
        fill_result.starved
    }

    /// Moves the buffered fill back to the target and resets the controller.
    ///
    /// Excess frames are dropped from the capture FIFO first, then from the staging FIFO. A
    /// shortfall is filled with silence in the staging FIFO.
    fn resync_to_target(&mut self, src: &mut impl FrameSource) {
        let buffered = src.len_frames() + self.resampler.staged_frames();
        let target = self.target_frames;
        if buffered > target {
            let excess = buffered - target;
            let from_src = excess.min(src.len_frames());
            src.discard(from_src);
            self.resampler.discard_staged(excess - from_src);
        } else if buffered < target {
            self.resampler.pad_staged_silence(target - buffered);
        }
        self.drift.resync();
        self.stats.add_resync();
    }

    /// Up-mixes the scratch block (stereo) into `out` (`out_channels` per frame).
    ///
    /// One output channel gets `(L + R) / 2`. Otherwise channel 0 is L, channel 1 is R and all
    /// further channels are silent.
    #[allow(
        clippy::chunks_exact_to_as_chunks,
        reason = "slice::as_chunks needs Rust 1.88; the workspace MSRV is 1.85"
    )]
    fn write_output(&self, out: &mut [f32]) {
        let channels = self.out_channels;
        for (frame, stereo) in out
            .chunks_exact_mut(channels)
            .zip(self.scratch.chunks_exact(CHANNELS))
        {
            if channels == 1 {
                frame[0] = f32::midpoint(stereo[0], stereo[1]);
            } else {
                frame[0] = stereo[0];
                frame[1] = stereo[1];
                frame[2..].fill(0.0);
            }
        }
    }

    /// Publishes the buffered audio plus the device latency to the statistics.
    fn publish_latency(&self, src_len: usize) {
        let buffered = src_len + self.resampler.staged_frames();
        let ms = frames_to_ms(buffered, self.sample_rate) + self.device_latency_ms;
        self.stats.set_latency_ms(ms);
    }
}

/// Bitwise equality of two gain slices. Gains are compared as bits, so a change is detected
/// exactly and no float comparison is involved.
#[must_use]
fn same_gains(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

/// Converts a frame count at `sample_rate` into milliseconds.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "frame counts are far below 2^53, so the conversion to f64 is exact"
)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "latency is reported as f32 for the UI; the rounding is intended"
)]
fn frames_to_ms(frames: usize, sample_rate: f64) -> f32 {
    (frames as f64 * 1000.0 / sample_rate) as f32
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "tests convert small, known sample and frame counts"
)]
mod tests {
    use super::*;
    use crate::fifo::StereoFifo;
    use crate::shared::params_channel;

    const FS: f64 = 48_000.0;
    /// 30 ms at 48 kHz.
    const TARGET: usize = 1_440;
    /// Render period used by the tests: 10 ms.
    const PERIOD: usize = 480;

    /// A processor wired to a capture FIFO, with the input clock equal to the render clock.
    struct Rig {
        processor: Processor,
        writer: crate::shared::ParamsWriter,
        stats: Arc<SharedStats>,
        src: StereoFifo,
        next: usize,
        channels: usize,
    }

    impl Rig {
        fn new(channels: usize) -> Self {
            let (writer, reader) = params_channel(EqParams::default());
            let stats = Arc::new(SharedStats::new());
            let config = ProcessorConfig {
                sample_rate: FS,
                out_channels: channels,
                fifo_target_frames: TARGET,
                max_block_frames: 512,
            };
            let processor =
                Processor::new(config, reader, Arc::clone(&stats)).expect("valid config");
            Self {
                processor,
                writer,
                stats,
                src: StereoFifo::with_capacity_frames(FS as usize),
                next: 0,
                channels,
            }
        }

        /// Pushes `frames` input frames without rendering, to build up the initial fill.
        fn prime(&mut self, frames: usize, signal: fn(usize) -> [f32; 2]) {
            let block = Self::block(self.next, frames, signal);
            assert_eq!(self.src.push(&block), frames);
            self.next += frames;
        }

        /// Pushes one period of input and renders one period of output.
        fn period(&mut self, signal: fn(usize) -> [f32; 2]) -> Vec<f32> {
            self.prime(PERIOD, signal);
            let mut out = vec![0.0; PERIOD * self.channels];
            self.processor.render(&mut self.src, &mut out);
            out
        }

        fn block(start: usize, frames: usize, signal: fn(usize) -> [f32; 2]) -> Vec<f32> {
            (start..start + frames).flat_map(signal).collect()
        }
    }

    /// Stereo sine at 100 Hz, amplitude 0.5, both channels equal.
    fn sine_100(n: usize) -> [f32; 2] {
        let v = (2.0 * std::f64::consts::PI * 100.0 * n as f64 / FS).sin() as f32 * 0.5;
        [v, v]
    }

    /// Stereo sine at 1 kHz, amplitude 0.05, both channels equal.
    fn sine_1k_quiet(n: usize) -> [f32; 2] {
        let v = (2.0 * std::f64::consts::PI * 1_000.0 * n as f64 / FS).sin() as f32 * 0.05;
        [v, v]
    }

    /// Root-mean-square of a slice.
    fn rms(v: &[f32]) -> f64 {
        let sum: f64 = v.iter().map(|&x| f64::from(x) * f64::from(x)).sum();
        (sum / v.len() as f64).sqrt()
    }

    #[test]
    fn flat_eq_with_equal_clocks_delays_the_input_unchanged() {
        let mut rig = Rig::new(2);
        rig.prime(TARGET - PERIOD, sine_100);
        let mut out = Vec::new();
        for _ in 0..300 {
            out.extend(rig.period(sine_100));
        }

        // Find the integer delay that matches the output window best, then check the error.
        // The cubic resampler is not bit-exact and the fill moves within a period, so the
        // tolerance is set by one sample of misalignment at 100 Hz (about 3.3e-3 for 0.5 peak).
        let input: Vec<f32> = (0..out.len() / 2).map(|n| sine_100(n)[0]).collect();
        let window = 48_000..60_000; // a quarter second, after the controller has settled
        let mut best = (usize::MAX, f32::INFINITY);
        for lag in 0..3_000 {
            let mut worst = 0.0_f32;
            for k in window.clone() {
                worst = worst.max((out[2 * k] - input[k - lag]).abs());
            }
            if worst < best.1 {
                best = (lag, worst);
            }
        }
        assert!(best.0 > 0 && best.0 < 3_000, "delay {} frames", best.0);
        assert!(best.1 < 6e-3, "max error {} at delay {}", best.1, best.0);
    }

    #[test]
    fn latency_is_published_from_buffered_frames() {
        let mut rig = Rig::new(2);
        rig.prime(TARGET - PERIOD, sine_100);
        for _ in 0..300 {
            let _ = rig.period(sine_100);
        }
        // The fill is steered to the target when a request starts; latency is published after
        // the request has consumed one period, so the steady value is target minus one period.
        let latency = rig.stats.snapshot().latency_ms;
        let expected_ms = (TARGET - PERIOD) as f32 * 1000.0 / FS as f32;
        assert!(
            (latency - expected_ms).abs() < 2.0,
            "latency {latency} ms, expected about {expected_ms} ms"
        );

        rig.processor.set_device_latency_ms(5.0);
        let _ = rig.period(sine_100);
        let with_device = rig.stats.snapshot().latency_ms;
        assert!(
            with_device >= 5.0 + latency - 2.0,
            "device latency not added: {with_device}"
        );
    }

    #[test]
    fn six_channel_output_carries_audio_only_in_first_two_channels() {
        let mut rig = Rig::new(6);
        rig.prime(TARGET - PERIOD, sine_100);
        let mut out = Vec::new();
        for _ in 0..100 {
            out.extend(rig.period(sine_100));
        }
        let frames = out.len() / 6;
        let left_peak = (0..frames).map(|i| out[i * 6].abs()).fold(0.0, f32::max);
        let right_peak = (0..frames)
            .map(|i| out[i * 6 + 1].abs())
            .fold(0.0, f32::max);
        assert!(left_peak > 0.4, "left peak {left_peak}");
        assert!(right_peak > 0.4, "right peak {right_peak}");
        for ch in 2..6 {
            assert!(
                (0..frames).all(|i| out[i * 6 + ch] == 0.0),
                "channel {ch} is not silent"
            );
        }
    }

    #[test]
    fn mono_output_averages_left_and_right() {
        // Left is a sine, right is silent: the mono mix must carry half the level.
        fn left_only(n: usize) -> [f32; 2] {
            [sine_100(n)[0], 0.0]
        }
        // Opposite channels cancel exactly in the mono mix.
        fn anti_phase(n: usize) -> [f32; 2] {
            let v = sine_100(n)[0];
            [v, -v]
        }
        let mut rig = Rig::new(1);
        rig.prime(TARGET - PERIOD, left_only);
        let mut out = Vec::new();
        for _ in 0..100 {
            out.extend(rig.period(left_only));
        }
        let peak = out[out.len() / 2..]
            .iter()
            .fold(0.0_f32, |m, &x| m.max(x.abs()));
        assert!(
            (peak - 0.25).abs() < 0.01,
            "mono peak {peak}, expected about 0.25"
        );

        let mut rig = Rig::new(1);
        rig.prime(TARGET - PERIOD, anti_phase);
        let mut out = Vec::new();
        for _ in 0..100 {
            out.extend(rig.period(anti_phase));
        }
        let peak = out[out.len() / 2..]
            .iter()
            .fold(0.0_f32, |m, &x| m.max(x.abs()));
        assert!(peak < 1e-4, "anti-phase mono peak {peak}");
    }

    #[test]
    fn starving_source_renders_silence_and_counts_one_underrun_per_call() {
        let mut rig = Rig::new(2);
        let mut out = vec![1.0_f32; PERIOD * 2];
        rig.processor.render(&mut rig.src, &mut out);
        assert!(out.iter().all(|&x| x == 0.0), "output must be silence");
        assert_eq!(rig.stats.snapshot().underruns, 1);

        // The empty call primed one target of silence, which covers the next three periods
        // without an underrun. Once it is used up, the next empty call underruns again.
        for _ in 0..3 {
            rig.processor.render(&mut rig.src, &mut out);
        }
        assert_eq!(rig.stats.snapshot().underruns, 1);
        rig.processor.render(&mut rig.src, &mut out);
        assert_eq!(rig.stats.snapshot().underruns, 2);
    }

    #[test]
    fn parameter_change_raises_1khz_level_by_the_boost() {
        let mut rig = Rig::new(2);
        rig.prime(TARGET - PERIOD, sine_1k_quiet);
        let mut before = Vec::new();
        for _ in 0..100 {
            before.extend(rig.period(sine_1k_quiet));
        }

        let boosted = EqParams {
            gains: {
                let mut g = [0.0; eq_dsp::MAX_BANDS];
                g[5] = 12.0; // the 1 kHz band
                g
            },
            ..EqParams::default()
        };
        rig.writer.publish(boosted);

        // Let the latency and the 30 ms gain ramp pass, then measure.
        let mut after = Vec::new();
        for _ in 0..100 {
            after.extend(rig.period(sine_1k_quiet));
        }
        let before_db = 20.0 * rms(&before[before.len() - 2 * 9_600..]).log10();
        let after_db = 20.0 * rms(&after[after.len() - 2 * 9_600..]).log10();
        assert!(
            after_db - before_db > 10.0,
            "1 kHz level rose by {} dB, expected about 12",
            after_db - before_db
        );
    }

    #[test]
    fn disabling_through_params_passes_the_signal_through() {
        let mut rig = Rig::new(2);
        let off = EqParams {
            enabled: false,
            ..EqParams::default()
        };
        rig.writer.publish(off);
        rig.prime(TARGET - PERIOD, sine_100);
        let mut out = Vec::new();
        for _ in 0..50 {
            out.extend(rig.period(sine_100));
        }
        // The EQ is bypassed, so the level matches the input (0.5 peak), not a limited value.
        let peak = out[out.len() / 2..]
            .iter()
            .fold(0.0_f32, |m, &x| m.max(x.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
    }

    #[test]
    fn invalid_sample_rate_is_rejected() {
        let (_writer, reader) = params_channel(EqParams::default());
        let config = ProcessorConfig {
            sample_rate: 0.0,
            out_channels: 2,
            fifo_target_frames: TARGET,
            max_block_frames: 512,
        };
        let result = Processor::new(config, reader, Arc::new(SharedStats::new()));
        assert!(matches!(result, Err(ProcessorError::InvalidSampleRate(_))));
    }
}
