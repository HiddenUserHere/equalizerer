//! Linux backend for Equalizerer: `PipeWire` registry and metadata, the runtime null sink
//! "Equalizerer", and the capture and playback streams.
//!
//! The crate is empty on other operating systems.

#![cfg(target_os = "linux")]
