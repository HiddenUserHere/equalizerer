//! Pure helpers for the WASAPI pipeline: stream formats, latency-mode periods and unit
//! conversions.
//!
//! Nothing here calls COM, so the unit tests run on any Windows machine without audio devices.

use eq_core::LatencyMode;
use windows::Win32::Media::Audio::{WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0};
use windows::Win32::Media::KernelStreaming::{SPEAKER_FRONT_LEFT, SPEAKER_FRONT_RIGHT};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};

/// `WAVE_FORMAT_EXTENSIBLE` as the 16-bit `wFormatTag` field.
const FORMAT_TAG_EXTENSIBLE: u16 = 0xFFFE;
/// `cbSize` of a `WAVEFORMATEXTENSIBLE` (the size of the extension after `WAVEFORMATEX`).
const EXTENSION_SIZE: u16 = 22;
/// Bytes per float32 sample.
const FLOAT_BYTES: u16 = 4;
/// Channels of the capture stream: the loopback stream is always stereo.
const CAPTURE_CHANNELS: u16 = 2;
/// Hundred-nanosecond units per millisecond.
const HNS_PER_MS: i64 = 10_000;
/// Hundred-nanosecond units per second.
const HNS_PER_SEC: i64 = 10_000_000;
/// Smallest FIFO target in the lower-latency mode, in frames (2 × 64).
const LOW_MIN_FIFO_FRAMES: usize = 128;
/// FIFO target of the automatic mode, in milliseconds.
const AUTO_FIFO_MS: usize = 30;
/// FIFO target of the more-reliable mode, in milliseconds.
const RELIABLE_FIFO_MS: usize = 80;

/// The stream parameters that the render side needs from a validated device format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FloatFormat {
    /// Sample rate of the device in Hz.
    pub(crate) sample_rate: u32,
    /// Number of interleaved channels of the device.
    pub(crate) channels: u16,
}

/// Checks that a device mix format is 32-bit IEEE float and returns its parameters.
///
/// `base` is the `WAVEFORMATEX` header. `extensible` must be `Some` when the header is a
/// `WAVEFORMATEXTENSIBLE` (format tag `0xFFFE` with an extension); it carries the sub-format.
///
/// # Errors
///
/// Returns a description of the format when it is not 32-bit float, or has no channels.
pub(crate) fn check_float32(
    base: &WAVEFORMATEX,
    extensible: Option<&WAVEFORMATEXTENSIBLE>,
) -> Result<FloatFormat, String> {
    let tag = base.wFormatTag;
    let bits = base.wBitsPerSample;
    let channels = base.nChannels;
    let is_float = match u32::from(tag) {
        WAVE_FORMAT_IEEE_FLOAT => true,
        _ => extensible.is_some_and(|ext| {
            // Copy the GUID out first: comparing a field of a packed struct takes a reference.
            let sub_format = ext.SubFormat;
            sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        }),
    };
    if !is_float || bits != 32 || channels == 0 {
        return Err(format!(
            "format tag 0x{tag:04X}, {bits} bits, {channels} channels (need 32-bit float)"
        ));
    }
    Ok(FloatFormat {
        sample_rate: base.nSamplesPerSec,
        channels,
    })
}

/// Builds the float32 stereo format requested from the loopback capture client.
///
/// The capture client converts the virtual device's mix format to this format, at the sample
/// rate of the render device.
#[must_use]
pub(crate) fn capture_format(sample_rate: u32) -> WAVEFORMATEXTENSIBLE {
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: FORMAT_TAG_EXTENSIBLE,
            nChannels: CAPTURE_CHANNELS,
            nSamplesPerSec: sample_rate,
            nAvgBytesPerSec: sample_rate * u32::from(FLOAT_BYTES * CAPTURE_CHANNELS),
            nBlockAlign: FLOAT_BYTES * CAPTURE_CHANNELS,
            wBitsPerSample: 32,
            cbSize: EXTENSION_SIZE,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: 32,
        },
        dwChannelMask: SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

/// Returns the FIFO fill target for `mode`, in frames.
///
/// The lower-latency target follows the engine period (1.5 periods, at least 128 frames); the
/// other two modes use fixed durations so that they keep their meaning on any device.
#[must_use]
pub(crate) fn fifo_target_frames(
    mode: LatencyMode,
    period_frames: usize,
    sample_rate: u32,
) -> usize {
    match mode {
        LatencyMode::Low => (period_frames * 3 / 2).max(LOW_MIN_FIFO_FRAMES),
        LatencyMode::Auto => ms_to_frames(AUTO_FIFO_MS, sample_rate),
        LatencyMode::Reliable => ms_to_frames(RELIABLE_FIFO_MS, sample_rate),
    }
}

/// Converts whole milliseconds into frames at `sample_rate`, rounding down.
#[must_use]
fn ms_to_frames(ms: usize, sample_rate: u32) -> usize {
    (sample_rate as usize) * ms / 1000
}

/// Converts a duration in hundred-nanosecond units into frames at `sample_rate`.
///
/// Negative durations count as zero.
#[must_use]
pub(crate) fn hns_to_frames(hns: i64, sample_rate: u32) -> usize {
    let hns = hns.max(0);
    usize::try_from(hns * i64::from(sample_rate) / HNS_PER_SEC).unwrap_or(usize::MAX)
}

/// Converts a duration in hundred-nanosecond units into milliseconds.
///
/// Negative durations count as zero. Sub-millisecond precision is kept.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    reason = "latency values are far below 2^53 hundred-nanoseconds, so the f64 is exact"
)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "latency is reported as f32 for the UI; the rounding is intended"
)]
pub(crate) fn hns_to_ms(hns: i64) -> f32 {
    (hns.max(0) as f64 / HNS_PER_MS as f64) as f32
}

/// Converts a frame count at `sample_rate` into milliseconds.
#[must_use]
#[allow(
    clippy::cast_possible_truncation,
    reason = "latency is reported as f32 for the UI; the rounding is intended"
)]
pub(crate) fn frames_to_ms(frames: u32, sample_rate: u32) -> f32 {
    (f64::from(frames) * 1000.0 / f64::from(sample_rate)) as f32
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "the conversions below are exact for the chosen inputs"
)]
mod tests {
    use super::*;

    /// Copies a packed field out of a packed struct. References to packed fields are not allowed.
    macro_rules! field {
        ($value:expr, $field:ident) => {{
            let copy = $value;
            copy.$field
        }};
    }

    fn float_header(channels: u16, bits: u16, tag: u16) -> WAVEFORMATEX {
        WAVEFORMATEX {
            wFormatTag: tag,
            nChannels: channels,
            nSamplesPerSec: 48_000,
            wBitsPerSample: bits,
            ..WAVEFORMATEX::default()
        }
    }

    #[test]
    fn capture_format_is_float32_stereo_extensible() {
        let format = capture_format(44_100);
        let header = field!(format, Format);
        assert_eq!(u32::from(field!(header, wFormatTag)), 0xFFFE);
        assert_eq!(field!(header, nChannels), 2);
        assert_eq!(field!(header, nSamplesPerSec), 44_100);
        assert_eq!(field!(header, nAvgBytesPerSec), 44_100 * 8);
        assert_eq!(field!(header, nBlockAlign), 8);
        assert_eq!(field!(header, wBitsPerSample), 32);
        assert_eq!(field!(header, cbSize), 22);
        let valid = field!(format, Samples);
        // SAFETY: the union is built with `wValidBitsPerSample`, so that member is the one
        // that holds the value. The copy is taken from a local, so no packed reference is made.
        assert_eq!(unsafe { field!(valid, wValidBitsPerSample) }, 32);
        assert_eq!(field!(format, dwChannelMask), 3);
        assert_eq!(field!(format, SubFormat), KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
    }

    #[test]
    fn capture_format_tag_matches_the_windows_constant() {
        assert_eq!(
            u32::from(FORMAT_TAG_EXTENSIBLE),
            windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE
        );
    }

    #[test]
    fn accepts_plain_ieee_float_header() {
        let header = float_header(2, 32, 3);
        let format = check_float32(&header, None).expect("float32 accepted");
        assert_eq!(
            format,
            FloatFormat {
                sample_rate: 48_000,
                channels: 2
            }
        );
    }

    #[test]
    fn accepts_extensible_float_subformat() {
        let header = float_header(6, 32, FORMAT_TAG_EXTENSIBLE);
        let ext = capture_format(48_000);
        let format = check_float32(&header, Some(&ext)).expect("float32 extensible accepted");
        assert_eq!(format.channels, 6);
    }

    #[test]
    fn rejects_pcm_16_bit() {
        let header = float_header(2, 16, 1);
        assert!(check_float32(&header, None).is_err());
    }

    #[test]
    fn rejects_extensible_with_non_float_subformat() {
        let header = float_header(2, 32, FORMAT_TAG_EXTENSIBLE);
        let mut ext = capture_format(48_000);
        ext.SubFormat = windows::core::GUID::from_u128(0);
        assert!(check_float32(&header, Some(&ext)).is_err());
    }

    #[test]
    fn rejects_zero_channels() {
        let header = float_header(0, 32, 3);
        assert!(check_float32(&header, None).is_err());
    }

    #[test]
    fn fifo_target_for_each_mode() {
        // 10 ms engine period at 48 kHz: 1.5 periods is 720 frames.
        assert_eq!(fifo_target_frames(LatencyMode::Low, 480, 48_000), 720);
        // Tiny periods are clamped to 2 × 64 frames.
        assert_eq!(fifo_target_frames(LatencyMode::Low, 64, 48_000), 128);
        assert_eq!(fifo_target_frames(LatencyMode::Auto, 480, 48_000), 1_440);
        assert_eq!(
            fifo_target_frames(LatencyMode::Reliable, 480, 48_000),
            3_840
        );
        // Fixed durations follow the sample rate.
        assert_eq!(fifo_target_frames(LatencyMode::Auto, 0, 44_100), 1_323);
    }

    #[test]
    fn hns_converts_to_frames() {
        // 10 ms, 100 ms and 200 ms (the capture buffer).
        assert_eq!(hns_to_frames(100_000, 48_000), 480);
        assert_eq!(hns_to_frames(1_000_000, 48_000), 4_800);
        assert_eq!(hns_to_frames(2_000_000, 44_100), 8_820);
        assert_eq!(hns_to_frames(-5, 48_000), 0);
    }

    #[test]
    fn hns_converts_to_milliseconds() {
        assert_eq!(hns_to_ms(100_000), 10.0);
        assert_eq!(hns_to_ms(15_000), 1.5);
        assert_eq!(hns_to_ms(-1), 0.0);
    }

    #[test]
    fn frames_convert_to_milliseconds() {
        assert_eq!(frames_to_ms(480, 48_000), 10.0);
        assert_eq!(frames_to_ms(0, 48_000), 0.0);
    }
}
