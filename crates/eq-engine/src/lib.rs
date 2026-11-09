//! Platform-agnostic real-time building blocks: the sample FIFO, the drift controller and
//! resampler, the audio processor (EQ, up-mix and metering), the parameter and stats handoff,
//! and the denormal guard.
//!
//! Code here runs on the audio thread, so it must not allocate, lock, log or block.

pub mod denormal;
pub mod drift;
pub mod fifo;
pub mod processor;
pub mod shared;

pub use denormal::DenormalGuard;
pub use drift::{DriftAction, DriftConfig, DriftController, DriftResampler, FillResult};
pub use fifo::{FifoConsumer, FifoProducer, FrameSource, StereoFifo, split_spsc};
pub use processor::{Processor, ProcessorConfig, ProcessorError};
pub use shared::{EngineStats, ParamsReader, ParamsWriter, SharedStats, params_channel};
