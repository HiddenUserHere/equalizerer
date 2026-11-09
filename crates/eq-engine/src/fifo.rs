//! Sample FIFOs between the capture side and the render side of the pipeline.
//!
//! Two flavours share the [`FrameSource`] trait:
//!
//! - [`StereoFifo`]: single-threaded ring buffer, used where capture and render run on the same
//!   thread (Windows: the render loop polls the loopback client).
//! - [`split_spsc`]: lock-free single-producer/single-consumer pair built on `rtrb`, used where
//!   capture and render run in different callbacks (Linux: `PipeWire` capture and playback).
//!
//! All frames are interleaved stereo `f32` (`[L, R, L, R, ...]`). Both types allocate only at
//! construction; every other method is allocation-free and never blocks.

/// Number of `f32` samples in one interleaved stereo frame.
const CHANNELS: usize = 2;

/// A source of interleaved stereo frames that the processor pulls from.
///
/// Implemented by [`StereoFifo`] and [`FifoConsumer`].
pub trait FrameSource {
    /// Number of whole frames currently available for reading.
    fn len_frames(&self) -> usize;

    /// Copies as many whole frames as fit into `out` and returns how many frames were read.
    ///
    /// A trailing odd sample in `out` is left untouched.
    fn pop_into(&mut self, out: &mut [f32]) -> usize;

    /// Drops up to `frames` frames from the front of the FIFO without copying them.
    fn discard(&mut self, frames: usize);
}

/// Single-threaded ring buffer of interleaved stereo frames.
///
/// The storage is allocated once in [`StereoFifo::with_capacity_frames`]. When full, [`push`]
/// writes only the frames that fit and reports how many were accepted; the rest is dropped.
///
/// [`push`]: StereoFifo::push
#[derive(Debug)]
pub struct StereoFifo {
    /// `capacity_frames * 2` samples.
    buf: Box<[f32]>,
    /// Capacity in frames.
    capacity: usize,
    /// Index of the oldest frame.
    read: usize,
    /// Number of frames stored.
    len: usize,
}

impl StereoFifo {
    /// Creates an empty FIFO that holds up to `frames` frames.
    ///
    /// This is the only allocation the type ever performs.
    #[must_use]
    pub fn with_capacity_frames(frames: usize) -> Self {
        Self {
            buf: vec![0.0; frames * CHANNELS].into_boxed_slice(),
            capacity: frames,
            read: 0,
            len: 0,
        }
    }

    /// Maximum number of frames the FIFO can hold.
    #[must_use]
    pub const fn capacity_frames(&self) -> usize {
        self.capacity
    }

    /// Number of frames currently stored.
    #[must_use]
    pub const fn len_frames(&self) -> usize {
        self.len
    }

    /// Number of frames that can still be pushed without dropping anything.
    #[must_use]
    pub const fn free_frames(&self) -> usize {
        self.capacity - self.len
    }

    /// Appends whole frames from `input` and returns the number of frames written.
    ///
    /// Frames that do not fit are dropped; a trailing odd sample is ignored.
    #[must_use]
    pub fn push(&mut self, input: &[f32]) -> usize {
        let frames = (input.len() / CHANNELS).min(self.free_frames());
        let start = self.wrap(self.read + self.len);
        self.write_frames(start, &input[..frames * CHANNELS]);
        self.len += frames;
        frames
    }

    /// Appends up to `frames` frames of silence, limited to the free space, and returns the
    /// number of frames written.
    ///
    /// Used to fill a gap on purpose (for example after a resync). Like [`push`](Self::push),
    /// it never overwrites stored frames.
    #[must_use]
    pub fn pad_silence(&mut self, frames: usize) -> usize {
        let frames = frames.min(self.free_frames());
        let start = self.wrap(self.read + self.len);
        // The ring may still hold stale audio in the target range, so zero it explicitly.
        let first = frames.min(self.capacity - start);
        self.buf[start * CHANNELS..(start + first) * CHANNELS].fill(0.0);
        self.buf[..(frames - first) * CHANNELS].fill(0.0);
        self.len += frames;
        frames
    }

    /// Copies up to `out.len() / 2` frames from the front into `out` and returns the number
    /// of frames read.
    #[must_use]
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let frames = (out.len() / CHANNELS).min(self.len);
        self.read_frames(self.read, &mut out[..frames * CHANNELS]);
        self.discard(frames);
        frames
    }

    /// Drops up to `frames` frames from the front.
    pub fn discard(&mut self, frames: usize) {
        let frames = frames.min(self.len);
        self.read = self.wrap(self.read + frames);
        self.len -= frames;
    }

    /// Removes all stored frames. The storage is kept.
    pub fn clear(&mut self) {
        self.read = 0;
        self.len = 0;
    }

    /// Maps an index in `0..2 * capacity` onto `0..capacity`.
    const fn wrap(&self, index: usize) -> usize {
        if index >= self.capacity {
            index - self.capacity
        } else {
            index
        }
    }

    /// Writes `src` (whole frames, `frames * 2` samples) starting at frame `start`, wrapping
    /// around the end of the buffer. The caller guarantees there is room.
    fn write_frames(&mut self, start: usize, src: &[f32]) {
        let first = (src.len() / CHANNELS).min(self.capacity - start) * CHANNELS;
        self.buf[start * CHANNELS..start * CHANNELS + first].copy_from_slice(&src[..first]);
        self.buf[..src.len() - first].copy_from_slice(&src[first..]);
    }

    /// Reads `dst.len() / 2` frames starting at frame `start`, wrapping around the end of the
    /// buffer. The caller guarantees that the frames are stored.
    fn read_frames(&self, start: usize, dst: &mut [f32]) {
        let len = dst.len();
        let first = (len / CHANNELS).min(self.capacity - start) * CHANNELS;
        dst[..first].copy_from_slice(&self.buf[start * CHANNELS..start * CHANNELS + first]);
        dst[first..].copy_from_slice(&self.buf[..len - first]);
    }
}

impl FrameSource for StereoFifo {
    fn len_frames(&self) -> usize {
        StereoFifo::len_frames(self)
    }

    fn pop_into(&mut self, out: &mut [f32]) -> usize {
        StereoFifo::pop_into(self, out)
    }

    fn discard(&mut self, frames: usize) {
        StereoFifo::discard(self, frames);
    }
}

/// Producer half of an SPSC frame FIFO, created by [`split_spsc`].
///
/// Owned by the capture callback.
#[derive(Debug)]
pub struct FifoProducer {
    inner: rtrb::Producer<f32>,
}

impl FifoProducer {
    /// Appends whole frames from `input` and returns the number of frames written.
    ///
    /// Frames that do not fit are dropped; a trailing odd sample is ignored.
    #[must_use]
    pub fn push(&mut self, input: &[f32]) -> usize {
        let frames = (input.len() / CHANNELS).min(self.inner.slots() / CHANNELS);
        let (pushed, _) = self.inner.push_partial_slice(&input[..frames * CHANNELS]);
        pushed.len() / CHANNELS
    }
}

/// Consumer half of an SPSC frame FIFO, created by [`split_spsc`].
///
/// Owned by the playback callback. It implements [`FrameSource`]. It cannot insert silence:
/// after a resync it can only discard, because it does not own the write side. The
/// [`StereoFifo`] `pad_silence` method is only available on the single-threaded FIFO.
#[derive(Debug)]
pub struct FifoConsumer {
    inner: rtrb::Consumer<f32>,
}

impl FifoConsumer {
    /// Copies up to `out.len() / 2` frames into `out` and returns the number of frames read.
    #[must_use]
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let frames = (out.len() / CHANNELS).min(self.len_frames());
        let (popped, _) = self.inner.pop_partial_slice(&mut out[..frames * CHANNELS]);
        popped.len() / CHANNELS
    }

    /// Number of whole frames currently available for reading.
    #[must_use]
    pub fn len_frames(&self) -> usize {
        self.inner.slots() / CHANNELS
    }

    /// Drops up to `frames` frames from the front without copying them.
    pub fn discard(&mut self, frames: usize) {
        let frames = frames.min(self.len_frames());
        if let Ok(chunk) = self.inner.read_chunk(frames * CHANNELS) {
            chunk.commit_all();
        }
    }
}

impl FrameSource for FifoConsumer {
    fn len_frames(&self) -> usize {
        FifoConsumer::len_frames(self)
    }

    fn pop_into(&mut self, out: &mut [f32]) -> usize {
        FifoConsumer::pop_into(self, out)
    }

    fn discard(&mut self, frames: usize) {
        FifoConsumer::discard(self, frames);
    }
}

/// Creates a lock-free SPSC frame FIFO holding up to `capacity_frames` frames.
///
/// The ring is allocated here; the returned halves never allocate. Frames are never split:
/// the producer writes only whole frames and the consumer reads only whole frames.
/// A capacity of zero yields a FIFO that accepts nothing.
#[must_use]
pub fn split_spsc(capacity_frames: usize) -> (FifoProducer, FifoConsumer) {
    let (producer, consumer) = rtrb::RingBuffer::new(capacity_frames * CHANNELS);
    (
        FifoProducer { inner: producer },
        FifoConsumer { inner: consumer },
    )
}

#[cfg(test)]
#[allow(
    clippy::chunks_exact_to_as_chunks,
    reason = "slice::as_chunks needs Rust 1.88; the workspace MSRV is 1.85"
)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Frame `index` as a stereo pair that identifies it.
    #[allow(
        clippy::cast_precision_loss,
        reason = "test indices stay far below 2^24, where f32 is exact"
    )]
    fn frame(index: usize) -> [f32; 2] {
        [index as f32, -(index as f32)]
    }

    fn frames_of(range: std::ops::Range<usize>) -> Vec<f32> {
        range.flat_map(frame).collect()
    }

    #[test]
    fn push_then_pop_returns_frames_in_order() {
        let mut fifo = StereoFifo::with_capacity_frames(8);
        assert_eq!(fifo.push(&frames_of(0..5)), 5);
        assert_eq!(fifo.len_frames(), 5);
        assert_eq!(fifo.free_frames(), 3);

        let mut out = [0.0; 6];
        assert_eq!(fifo.pop_into(&mut out), 3);
        assert_eq!(out[..6], frames_of(0..3)[..]);
        assert_eq!(fifo.len_frames(), 2);
    }

    #[test]
    fn wrap_around_with_odd_sizes_matches_reference() {
        // Capacity 5 with odd push/pop sizes forces the ring to wrap many times.
        const CAPACITY: usize = 5;
        let mut fifo = StereoFifo::with_capacity_frames(CAPACITY);
        let mut model: VecDeque<[f32; 2]> = VecDeque::new();
        let mut next = 0_usize;
        let mut seed: u32 = 0x1234_5678;
        for _ in 0..20_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let op = (seed >> 16) % 3;
            let count = ((seed >> 8) % 4) as usize + 1; // 1..=4 frames
            match op {
                0 => {
                    let input: Vec<f32> = (next..next + count).flat_map(frame).collect();
                    let expected = count.min(CAPACITY - model.len());
                    assert_eq!(fifo.push(&input), expected);
                    model.extend((next..next + expected).map(frame));
                    next += count;
                }
                1 => {
                    let mut out = vec![0.0; count * 2];
                    let read = fifo.pop_into(&mut out);
                    assert_eq!(read, count.min(model.len()));
                    for pair in out.chunks_exact(2).take(read) {
                        let want = model.pop_front().expect("model holds the frame");
                        assert_eq!(pair, &want[..]);
                    }
                }
                _ => {
                    fifo.discard(count);
                    for _ in 0..count.min(model.len()) {
                        model.pop_front();
                    }
                }
            }
            assert_eq!(fifo.len_frames(), model.len());
            assert_eq!(fifo.free_frames(), CAPACITY - model.len());
        }
    }

    #[test]
    fn push_beyond_capacity_truncates() {
        let mut fifo = StereoFifo::with_capacity_frames(4);
        assert_eq!(fifo.push(&frames_of(0..6)), 4);
        assert_eq!(fifo.len_frames(), 4);
        assert_eq!(fifo.free_frames(), 0);
        assert_eq!(fifo.push(&frames_of(10..12)), 0);

        let mut out = [0.0; 8];
        assert_eq!(fifo.pop_into(&mut out), 4);
        assert_eq!(out, frames_of(0..4)[..]);
    }

    #[test]
    fn odd_trailing_sample_is_ignored() {
        let mut fifo = StereoFifo::with_capacity_frames(4);
        assert_eq!(fifo.push(&[1.0, 2.0, 3.0]), 1);
        let mut out = [0.0; 3];
        assert_eq!(fifo.pop_into(&mut out), 1);
        assert_eq!(out, [1.0, 2.0, 0.0]);
    }

    #[test]
    fn pad_silence_writes_zeros_and_respects_free_space() {
        let mut fifo = StereoFifo::with_capacity_frames(4);
        assert_eq!(fifo.push(&frames_of(1..4)), 3); // one frame free
        assert_eq!(fifo.pad_silence(3), 1);
        let mut out = [9.0; 8];
        assert_eq!(fifo.pop_into(&mut out), 4);
        assert_eq!(out, [1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 0.0, 0.0]);
    }

    #[test]
    fn pad_silence_after_wrap_clears_stale_samples() {
        let mut fifo = StereoFifo::with_capacity_frames(4);
        assert_eq!(fifo.push(&frames_of(1..5)), 4); // full, stale data everywhere
        fifo.discard(3); // one frame left, read index is 3
        assert_eq!(fifo.pad_silence(3), 3); // writes across the wrap point
        let mut out = [9.0; 8];
        assert_eq!(fifo.pop_into(&mut out), 4);
        assert_eq!(out, [4.0, -4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn discard_and_clear() {
        let mut fifo = StereoFifo::with_capacity_frames(4);
        assert_eq!(fifo.push(&frames_of(0..4)), 4);
        fifo.discard(10);
        assert_eq!(fifo.len_frames(), 0);
        assert_eq!(fifo.push(&frames_of(7..9)), 2);
        fifo.clear();
        assert_eq!(fifo.len_frames(), 0);
        assert_eq!(fifo.capacity_frames(), 4);
    }

    #[test]
    fn spsc_basic_push_pop_and_discard() {
        let (mut producer, mut consumer) = split_spsc(4);
        assert_eq!(producer.push(&frames_of(0..6)), 4);
        assert_eq!(consumer.len_frames(), 4);
        consumer.discard(1);
        assert_eq!(producer.push(&frames_of(4..6)), 1);
        let mut out = [0.0; 8];
        assert_eq!(consumer.pop_into(&mut out), 4);
        assert_eq!(out, frames_of(1..5)[..]);
    }

    #[test]
    fn spsc_preserves_order_across_threads() {
        const FRAMES: usize = 1_000_000;
        let (mut producer, mut consumer) = split_spsc(4096);

        let writer = std::thread::spawn(move || {
            let mut chunk = [0.0_f32; 64];
            let mut next = 0_usize;
            while next < FRAMES {
                let count = (FRAMES - next).min(32);
                for i in 0..count {
                    chunk[i * 2..i * 2 + 2].copy_from_slice(&frame(next + i));
                }
                let mut offset = 0;
                while offset < count {
                    let pushed = producer.push(&chunk[offset * 2..count * 2]);
                    offset += pushed;
                    if pushed == 0 {
                        std::thread::yield_now();
                    }
                }
                next += count;
            }
        });

        let mut out = [0.0_f32; 64];
        let mut expected = 0_usize;
        while expected < FRAMES {
            let read = consumer.pop_into(&mut out);
            if read == 0 {
                std::thread::yield_now();
                continue;
            }
            for pair in out[..read * 2].chunks_exact(2) {
                assert_eq!(pair, &frame(expected)[..], "frame {expected} out of order");
                expected += 1;
            }
        }
        writer.join().expect("writer thread panicked");
        assert_eq!(consumer.len_frames(), 0);
    }

    #[test]
    fn both_fifos_implement_frame_source() {
        fn drain(source: &mut impl FrameSource) -> Vec<f32> {
            let mut out = vec![0.0; source.len_frames() * 2];
            assert_eq!(source.pop_into(&mut out) * 2, out.len());
            out
        }

        let mut fifo = StereoFifo::with_capacity_frames(3);
        assert_eq!(fifo.push(&frames_of(0..3)), 3);
        assert_eq!(drain(&mut fifo), frames_of(0..3));

        let (mut producer, mut consumer) = split_spsc(3);
        assert_eq!(producer.push(&frames_of(0..3)), 3);
        assert_eq!(drain(&mut consumer), frames_of(0..3));
    }
}
