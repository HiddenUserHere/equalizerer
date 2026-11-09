//! Pure DSP for Equalizerer: biquads, filter design, the 10-band graphic EQ,
//! band-interaction correction, smoothing, the peak limiter and the response curve.
//!
//! This crate is `#![no_std]` with no dependencies and an allocation-free hot path, so it
//! can be used from real-time audio threads on every platform.
//!
//! Tests run with `std` available; every non-test build is `no_std`.

#![cfg_attr(not(test), no_std)]

pub mod biquad;
pub mod correction;
pub mod design;
pub mod graphic_eq;
pub mod layout;
pub mod limiter;
pub mod params;
pub mod response;

pub use biquad::{Coeffs, StereoBiquad};
pub use correction::corrected_gains;
pub use design::peaking;
pub use graphic_eq::GraphicEq;
pub use layout::{GAIN_MAX_DB, GAIN_MIN_DB, Layout, MAX_BANDS};
pub use limiter::PeakLimiter;
pub use params::EqParams;
pub use response::{log_frequency, magnitude_db, response_curve};
