//! Clock-drift compensation between the capture clock and the render clock.
//!
//! The capture side (the virtual device) and the render side (the target device) run on two
//! independent clocks, so the FIFO between them slowly fills up or empties. This module keeps
//! the fill near the latency target by changing the rate of a polynomial resampler slightly:
//!
//! - [`DriftController`]: a PI controller on the smoothed fill error. It produces the resampling
//!   ratio (output frames per input frame) and reports when the fill is so far off that a resync
//!   is needed.
//! - [`DriftResampler`]: a `rubato` fixed-output resampler with its own staging FIFO. It pulls
//!   input from the capture FIFO on demand and hands out output frames in render-sized pieces.
//!
//! Sign convention: a positive fill error (more buffered than the target) lowers the ratio.
//! A ratio below 1.0 makes the resampler consume input faster, so the fill falls back towards
//! the target. A negative error raises the ratio above 1.0.
//!
//! Nothing here allocates after construction.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, PolynomialDegree, Resampler, ResamplerConstructionError,
};

use crate::fifo::{FrameSource, StereoFifo};

/// Channels in every buffer of this module.
const CHANNELS: usize = 2;

/// Default number of output frames produced per resampler call.
const DEFAULT_CHUNK_FRAMES: usize = 64;

/// Default largest deviation of the resampling ratio from 1.0 (0.5 %).
const DEFAULT_MAX_RATIO_DEVIATION: f64 = 0.005;

/// Time constant of the moving average over the fill, in seconds.
///
/// Short enough to follow a real drift within seconds, long enough that the 10 ms period jitter
/// of the FIFO does not reach the controller.
const SMOOTHING_SECONDS: f64 = 0.5;

/// A fill above this multiple of the target triggers a resync.
const RESYNC_FACTOR: usize = 3;

/// Proportional gain: ratio offset per unit of relative fill error (error / target).
///
/// Chosen with the simulation tests in this module: together with [`KI`] it gives a natural
/// frequency of about 0.5 rad/s and a damping ratio of about 0.7 with the 0.5 s moving average,
/// which settles in several seconds without overshoot.
const KP: f64 = 0.02;

/// Integral gain: ratio offset per unit of relative fill error integrated over seconds.
const KI: f64 = 0.0075;

/// Keeps a deviation limit finite and inside a sane range; NaN falls back to the default.
#[must_use]
fn sanitize_deviation(deviation: f64) -> f64 {
    if deviation.is_finite() {
        deviation.clamp(1e-6, 0.5)
    } else {
        DEFAULT_MAX_RATIO_DEVIATION
    }
}

/// Converts a frame count to `f64`. Frame counts are far below 2^53, so this is exact.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "frame counts are far below 2^53, so the conversion to f64 is exact"
)]
fn frames_f64(frames: usize) -> f64 {
    frames as f64
}

/// Configuration shared by [`DriftController`] and [`DriftResampler`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftConfig {
    /// Nominal sample rate of the render side in Hz. Time constants are measured in its frames.
    pub sample_rate: f64,
    /// Fill the controller steers to, in frames. This is the latency target.
    pub target_frames: usize,
    /// Output frames produced per resampler call.
    pub chunk_frames: usize,
    /// Largest allowed deviation of the resampling ratio from 1.0, as a fraction
    /// (`0.005` means ±0.5 %).
    pub max_ratio_deviation: f64,
}

impl DriftConfig {
    /// Creates a configuration with the default chunk size (64 frames) and ratio limit (±0.5 %).
    #[must_use]
    pub const fn new(sample_rate: f64, target_frames: usize) -> Self {
        Self {
            sample_rate,
            target_frames,
            chunk_frames: DEFAULT_CHUNK_FRAMES,
            max_ratio_deviation: DEFAULT_MAX_RATIO_DEVIATION,
        }
    }
}

/// What the caller should do about the buffered fill, as decided by [`DriftController::action`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftAction {
    /// The fill is acceptable; nothing to do.
    None,
    /// The buffered fill must be brought back to this many frames. The caller discards the
    /// excess (or pads silence when short) and then calls [`DriftController::resync`].
    ResyncTo(usize),
}

/// PI controller that turns the fill of the capture FIFO into a resampling ratio.
///
/// Each [`update`](Self::update) call moves a moving average of the fill towards the measured
/// value, computes the relative error `(average - target) / target`, and returns
/// `1.0 - clamp(KP * error + KI * integral, ±max_ratio_deviation)`. The integral only accumulates
/// while the output is not saturated in the direction of the error (anti-windup).
#[derive(Debug, Clone)]
pub struct DriftController {
    /// Sample rate used to convert elapsed frames into seconds.
    sample_rate: f64,
    /// Target fill in frames, at least 1.
    target_frames: usize,
    /// Target fill as `f64`, used for the relative error.
    target: f64,
    /// Largest allowed deviation of the ratio from 1.0.
    max_deviation: f64,
    /// Moving average of the fill, in frames.
    smoothed: f64,
    /// Integral of the relative error, in seconds.
    integral: f64,
}

impl DriftController {
    /// Creates a controller that starts at the target fill with no integral.
    ///
    /// The sample rate in `config` must be positive and finite; the caller checks this.
    #[must_use]
    pub fn new(config: &DriftConfig) -> Self {
        let target_frames = config.target_frames.max(1);
        Self {
            sample_rate: config.sample_rate,
            target_frames,
            target: frames_f64(target_frames),
            max_deviation: sanitize_deviation(config.max_ratio_deviation),
            smoothed: frames_f64(target_frames),
            integral: 0.0,
        }
    }

    /// Advances the controller by one render request and returns the resampling ratio.
    ///
    /// - `fill_frames`: frames buffered now (capture FIFO plus resampler staging).
    /// - `elapsed_frames`: frames of render time since the previous call. It sets the weight of
    ///   the moving average and the length of the integration step, so the result does not
    ///   depend on how often the caller runs.
    ///
    /// The ratio is `1.0 - u`: a positive error gives a ratio below 1.0 and consumes input faster.
    #[must_use]
    pub fn update(&mut self, fill_frames: usize, elapsed_frames: usize) -> f64 {
        let elapsed = frames_f64(elapsed_frames);
        let alpha = 1.0 - (-elapsed / (SMOOTHING_SECONDS * self.sample_rate)).exp();
        self.smoothed += alpha * (frames_f64(fill_frames) - self.smoothed);

        let error = (self.smoothed - self.target) / self.target;
        let dt = elapsed / self.sample_rate;
        let proportional = KP * error;

        // Anti-windup by conditional integration: freeze the integral while the output is
        // saturated and the error would push it further into saturation.
        let candidate = self.integral + error * dt;
        let raw = proportional + KI * candidate;
        let saturated_high = raw > self.max_deviation && error > 0.0;
        let saturated_low = raw < -self.max_deviation && error < 0.0;
        if !(saturated_high || saturated_low) {
            self.integral = candidate;
        }

        let u = (proportional + KI * self.integral).clamp(-self.max_deviation, self.max_deviation);
        1.0 - u
    }

    /// Decides whether the fill is so far off target that the caller must resync.
    ///
    /// Returns [`DriftAction::ResyncTo`] when `fill_frames` exceeds three times the target.
    /// An empty buffer is handled by the caller, which knows whether the source delivered
    /// anything during the request.
    #[must_use]
    pub const fn action(&self, fill_frames: usize) -> DriftAction {
        if fill_frames > RESYNC_FACTOR * self.target_frames {
            DriftAction::ResyncTo(self.target_frames)
        } else {
            DriftAction::None
        }
    }

    /// Resets the controller after the caller has moved the fill back to the target.
    ///
    /// The moving average returns to the target and the integral is cleared, so the stale
    /// error of the old fill does not leak into the next ratios.
    pub fn resync(&mut self) {
        self.smoothed = frames_f64(self.target_frames);
        self.integral = 0.0;
    }

    /// Current moving average of the fill, in frames. Useful for diagnostics.
    #[must_use]
    pub const fn smoothed_frames(&self) -> f64 {
        self.smoothed
    }
}

/// Result of [`DriftResampler::fill_output`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FillResult {
    /// Output frames added to the staging FIFO by this call.
    pub produced: usize,
    /// `true` when the source ran out of frames before the staging FIFO reached the requested
    /// size. The frames that were available are still staged.
    pub starved: bool,
}

/// Polynomial (cubic) fixed-output resampler with an output staging FIFO.
///
/// The resampler is created with a maximum ratio deviation so that its internal buffers can hold
/// the largest input request at any allowed ratio. Input and output buffers are allocated here
/// and reused by [`fill_output`](Self::fill_output).
#[derive(Debug)]
pub struct DriftResampler {
    /// Cubic polynomial resampler; output size is fixed per call, input size varies with the ratio.
    resampler: Async<f32>,
    /// Output frames per resampler call.
    chunk_frames: usize,
    /// Largest and smallest allowed relative ratio.
    max_ratio: f64,
    /// Interleaved stereo input scratch, sized for the largest input request.
    input: Vec<f32>,
    /// Interleaved stereo output scratch, one chunk.
    output: Vec<f32>,
    /// Resampled frames waiting for the render side.
    staging: StereoFifo,
}

impl DriftResampler {
    /// Creates a resampler that can stage up to `max_output_frames` frames for one request.
    ///
    /// Staging holds `max_output_frames` plus one chunk, so a request of `max_output_frames`
    /// frames never overflows it. All memory is allocated here.
    ///
    /// # Errors
    ///
    /// Returns the `rubato` construction error if the configuration is rejected (for example a
    /// zero chunk size is replaced by 1, so in practice this only fails on non-finite input).
    pub fn new(
        config: &DriftConfig,
        max_output_frames: usize,
    ) -> Result<Self, ResamplerConstructionError> {
        let chunk_frames = config.chunk_frames.max(1);
        let deviation = sanitize_deviation(config.max_ratio_deviation);
        let max_ratio = 1.0 + deviation;
        let resampler = Async::<f32>::new_poly(
            1.0,
            max_ratio,
            PolynomialDegree::Cubic,
            chunk_frames,
            CHANNELS,
            FixedAsync::Output,
        )?;
        let input_frames_max = resampler.input_frames_max();
        Ok(Self {
            resampler,
            chunk_frames,
            max_ratio,
            input: vec![0.0; input_frames_max * CHANNELS],
            output: vec![0.0; chunk_frames * CHANNELS],
            staging: StereoFifo::with_capacity_frames(max_output_frames + chunk_frames),
        })
    }

    /// Frames waiting in the staging FIFO.
    #[must_use]
    pub const fn staged_frames(&self) -> usize {
        self.staging.len_frames()
    }

    /// Resamples from `src` into the staging FIFO until it holds at least `needed_frames`.
    ///
    /// `ratio` is the output/input frame ratio for this request (1.0 is nominal); it is clamped to
    /// the range the resampler was created for and ramped over the chunks of this call. Each
    /// resampler call consumes exactly `input_frames_next()` frames from `src`. If `src` holds
    /// fewer frames than that, the call stops and reports `starved`; nothing is invented.
    ///
    /// `needed_frames` is limited to the capacity given to [`new`](Self::new).
    pub fn fill_output(
        &mut self,
        src: &mut impl FrameSource,
        ratio: f64,
        needed_frames: usize,
    ) -> FillResult {
        let needed = needed_frames.min(self.staging.capacity_frames() - self.chunk_frames);
        let ratio = if ratio.is_finite() {
            ratio.clamp(1.0 / self.max_ratio, self.max_ratio)
        } else {
            1.0
        };
        if self
            .resampler
            .set_resample_ratio_relative(ratio, true)
            .is_err()
        {
            // The ratio was clamped to the allowed range, so this is unreachable in practice.
            return FillResult {
                produced: 0,
                starved: true,
            };
        }

        let mut produced = 0;
        while self.staging.len_frames() < needed {
            let input_frames = self.resampler.input_frames_next();
            if input_frames * CHANNELS > self.input.len() || src.len_frames() < input_frames {
                return FillResult {
                    produced,
                    starved: true,
                };
            }
            let read = src.pop_into(&mut self.input[..input_frames * CHANNELS]);
            if read < input_frames {
                // The FrameSource contract makes this unreachable; keep the buffer defined anyway.
                self.input[read * CHANNELS..input_frames * CHANNELS].fill(0.0);
            }

            let Ok(input) = InterleavedSlice::new(
                &self.input[..input_frames * CHANNELS],
                CHANNELS,
                input_frames,
            ) else {
                return FillResult {
                    produced,
                    starved: true,
                };
            };
            let Ok(mut output) =
                InterleavedSlice::new_mut(&mut self.output[..], CHANNELS, self.chunk_frames)
            else {
                return FillResult {
                    produced,
                    starved: true,
                };
            };
            let Ok((_, out_frames)) = self
                .resampler
                .process_into_buffer(&input, &mut output, None)
            else {
                return FillResult {
                    produced,
                    starved: true,
                };
            };

            produced += self.staging.push(&self.output[..out_frames * CHANNELS]);
        }
        FillResult {
            produced,
            starved: false,
        }
    }

    /// Moves up to `out.len() / 2` staged frames into `out` and returns the number moved.
    pub fn pop_output(&mut self, out: &mut [f32]) -> usize {
        self.staging.pop_into(out)
    }

    /// Drops up to `frames` staged frames.
    pub fn discard_staged(&mut self, frames: usize) {
        self.staging.discard(frames);
    }

    /// Appends up to `frames` frames of silence to the staging FIFO and returns the number added.
    pub fn pad_staged_silence(&mut self, frames: usize) -> usize {
        self.staging.pad_silence(frames)
    }
}

#[cfg(test)]
#[allow(
    clippy::chunks_exact_to_as_chunks,
    reason = "slice::as_chunks needs Rust 1.88; the workspace MSRV is 1.85"
)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "test simulations convert small, known frame and period counts"
)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;
    /// 30 ms at 48 kHz, the Automatic latency target.
    const TARGET: usize = 1_440;
    /// Render period of the simulated device.
    const PERIOD_S: f64 = 0.010;
    /// Largest render request the simulations make, in frames.
    const MAX_REQUEST: usize = 1_024;

    /// Outcome of one simulated clock-drift run.
    struct SimResult {
        /// Largest relative distance of the buffered fill from the target after warm-up.
        max_fill_deviation: f64,
        /// Largest distance of the ratio from 1.0 after warm-up.
        max_ratio_deviation: f64,
        /// Number of periods in which the controller asked for a resync.
        resyncs: usize,
        /// Number of periods in which the resampler ran out of input.
        starved: usize,
    }

    /// Simulates a producer pushing `producer_hz` frames per second into the capture FIFO and a
    /// consumer requesting `consumer_hz` frames per second, both in 10 ms periods.
    fn simulate(producer_hz: f64, consumer_hz: f64, seconds: f64, warmup_s: f64) -> SimResult {
        let config = DriftConfig::new(FS, TARGET);
        let mut controller = DriftController::new(&config);
        let mut resampler = DriftResampler::new(&config, MAX_REQUEST).expect("valid config");
        let mut src = StereoFifo::with_capacity_frames(FS as usize);
        let block = vec![0.1_f32; MAX_REQUEST * CHANNELS];
        let mut out = vec![0.0_f32; MAX_REQUEST * CHANNELS];

        // Start with the fill at the target, as the pipeline does after priming.
        let period_frames = (consumer_hz * PERIOD_S).round() as usize;
        assert_eq!(
            src.push(&block[..(TARGET - period_frames) * CHANNELS]),
            TARGET - period_frames
        );

        let periods = (seconds / PERIOD_S).round() as usize;
        let warmup_periods = (warmup_s / PERIOD_S).round() as usize;
        let mut due_produce = 0.0_f64;
        let mut due_consume = 0.0_f64;
        let mut result = SimResult {
            max_fill_deviation: 0.0,
            max_ratio_deviation: 0.0,
            resyncs: 0,
            starved: 0,
        };

        for period in 0..periods {
            due_produce += producer_hz * PERIOD_S;
            let produced = due_produce.floor();
            due_produce -= produced;
            assert_eq!(
                src.push(&block[..(produced as usize) * CHANNELS]),
                produced as usize
            );

            due_consume += consumer_hz * PERIOD_S;
            let requested = due_consume.floor();
            due_consume -= requested;
            let requested = requested as usize;

            let fill = src.len_frames() + resampler.staged_frames();
            if period >= warmup_periods {
                let deviation = (fill as f64 - TARGET as f64).abs() / TARGET as f64;
                result.max_fill_deviation = result.max_fill_deviation.max(deviation);
            }
            if let DriftAction::ResyncTo(target) = controller.action(fill) {
                result.resyncs += 1;
                let excess = fill.saturating_sub(target);
                let from_src = excess.min(src.len_frames());
                src.discard(from_src);
                resampler.discard_staged(excess - from_src);
                controller.resync();
            }

            let ratio = controller.update(fill, requested);
            if period >= warmup_periods {
                result.max_ratio_deviation = result.max_ratio_deviation.max((ratio - 1.0).abs());
            }
            let fill_result = resampler.fill_output(&mut src, ratio, requested);
            if fill_result.starved {
                result.starved += 1;
            }
            let got = resampler.pop_output(&mut out[..requested * CHANNELS]);
            assert_eq!(got, requested, "period {period} delivered too few frames");
        }
        result
    }

    /// Interleaved stereo sine at `freq` Hz, frame `n`.
    fn sine_frame(n: usize, freq: f64) -> [f32; 2] {
        let v = (2.0 * std::f64::consts::PI * freq * n as f64 / FS).sin() as f32 * 0.5;
        [v, v]
    }

    #[test]
    fn controller_output_is_unity_at_target_and_signs_match_convention() {
        let config = DriftConfig::new(FS, TARGET);
        let mut ctl = DriftController::new(&config);
        assert_eq!(ctl.update(TARGET, 480), 1.0);

        // Too much buffered: ratio below 1.0 (consume faster).
        let mut ctl = DriftController::new(&config);
        let mut ratio = 1.0;
        for _ in 0..200 {
            ratio = ctl.update(TARGET * 2, 480);
        }
        assert!(
            ratio < 1.0,
            "ratio {ratio} should be below 1.0 when overfull"
        );

        // Too little buffered: ratio above 1.0 (consume slower).
        let mut ctl = DriftController::new(&config);
        for _ in 0..200 {
            ratio = ctl.update(TARGET / 2, 480);
        }
        assert!(
            ratio > 1.0,
            "ratio {ratio} should be above 1.0 when underfull"
        );
    }

    #[test]
    fn controller_ratio_stays_within_limit_and_anti_windup_recovers() {
        let config = DriftConfig::new(FS, TARGET);
        let mut ctl = DriftController::new(&config);
        // A long, huge error saturates the output; the integral must not wind up.
        for _ in 0..100_000 {
            let r = ctl.update(TARGET * 10, 480);
            assert!(
                (r - 1.0).abs() <= 0.005 + 1e-12,
                "ratio {r} outside the limit"
            );
        }
        // Once the fill is back at the target the output must leave saturation quickly.
        let mut ratio = 0.0;
        for _ in 0..2_000 {
            ratio = ctl.update(TARGET, 480);
        }
        assert!((ratio - 1.0).abs() < 1e-3, "ratio {ratio} did not recover");
    }

    #[test]
    fn resync_decision_and_reset() {
        let config = DriftConfig::new(FS, TARGET);
        let mut ctl = DriftController::new(&config);
        assert_eq!(ctl.action(3 * TARGET), DriftAction::None);
        assert_eq!(ctl.action(3 * TARGET + 1), DriftAction::ResyncTo(TARGET));
        for _ in 0..100 {
            let _ = ctl.update(10 * TARGET, 480);
        }
        ctl.resync();
        assert_eq!(ctl.smoothed_frames(), TARGET as f64);
        assert_eq!(ctl.update(TARGET, 0), 1.0);
    }

    #[test]
    fn equal_clocks_keep_ratio_at_unity_after_warmup() {
        let run = simulate(FS, FS, 60.0, 20.0);
        assert!(
            run.max_ratio_deviation <= 1e-4,
            "ratio deviated by {}",
            run.max_ratio_deviation
        );
        assert_eq!(run.resyncs, 0);
        assert_eq!(run.starved, 0);
    }

    #[test]
    fn consumer_faster_by_10_ppm_keeps_fill_near_target() {
        let run = simulate(48_000.0, 48_010.0, 600.0, 20.0);
        assert!(
            run.max_fill_deviation <= 0.20,
            "fill deviation {}",
            run.max_fill_deviation
        );
        assert_eq!(run.resyncs, 0);
        assert_eq!(run.starved, 0);
    }

    #[test]
    fn consumer_slower_by_10_ppm_keeps_fill_near_target() {
        let run = simulate(48_000.0, 47_990.0, 600.0, 20.0);
        assert!(
            run.max_fill_deviation <= 0.20,
            "fill deviation {}",
            run.max_fill_deviation
        );
        assert_eq!(run.resyncs, 0);
        assert_eq!(run.starved, 0);
    }

    #[test]
    fn producer_faster_by_10_ppm_keeps_fill_near_target() {
        let run = simulate(48_010.0, 48_000.0, 600.0, 20.0);
        assert!(
            run.max_fill_deviation <= 0.20,
            "fill deviation {}",
            run.max_fill_deviation
        );
        assert_eq!(run.resyncs, 0);
        assert_eq!(run.starved, 0);
    }

    #[test]
    fn unity_ratio_preserves_frequency() {
        let config = DriftConfig::new(FS, TARGET);
        let mut resampler = DriftResampler::new(&config, MAX_REQUEST).expect("valid config");
        let mut src = StereoFifo::with_capacity_frames(8_192);
        let mut next = 0_usize;
        let mut out: Vec<f32> = Vec::new();
        let mut buf = vec![0.0_f32; 480 * CHANNELS];

        while out.len() < 2 * 48_000 * CHANNELS {
            while src.free_frames() >= 480 {
                for (i, pair) in buf.chunks_exact_mut(CHANNELS).enumerate() {
                    pair.copy_from_slice(&sine_frame(next + i, 1_000.0));
                }
                assert_eq!(src.push(&buf), 480);
                next += 480;
            }
            let fill = resampler.fill_output(&mut src, 1.0, 480);
            assert!(!fill.starved);
            let got = resampler.pop_output(&mut buf);
            out.extend_from_slice(&buf[..got * CHANNELS]);
        }

        // Skip the resampler start-up and count zero crossings over one second.
        let window: Vec<f32> = out[24_000 * CHANNELS..(24_000 + 48_000) * CHANNELS]
            .chunks_exact(CHANNELS)
            .map(|f| f[0])
            .collect();
        let crossings = window
            .windows(2)
            .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
            .count();
        assert!(
            crossings.abs_diff(2_000) <= 1,
            "1 kHz sine has {crossings} zero crossings per second, expected 2000"
        );
    }

    #[test]
    fn fill_output_reports_starvation_without_inventing_frames() {
        let config = DriftConfig::new(FS, TARGET);
        let mut resampler = DriftResampler::new(&config, MAX_REQUEST).expect("valid config");
        let mut src = StereoFifo::with_capacity_frames(4_096);
        let result = resampler.fill_output(&mut src, 1.0, 480);
        assert!(result.starved);
        assert_eq!(result.produced, 0);
        assert_eq!(resampler.staged_frames(), 0);
    }
}
