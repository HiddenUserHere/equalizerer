//! Domain layer of Equalizerer: the model, the ports (traits) the OS backends implement,
//! the router state machine, presets, settings and the controller.
//!
//! This crate must not depend on OS-specific crates and must build on every platform.
//! The router is a pure state machine: it receives inputs and returns effects, and the
//! controller performs the I/O.

pub mod controller;
pub mod locator;
pub mod messages;
pub mod model;
pub mod ports;
pub mod presets;
pub mod router;
pub mod settings;

pub use controller::{Controller, ControllerConfig};
pub use locator::PatternLocator;
pub use messages::{AppCommand, AppEvent, AppSnapshot, DeviceEvent, PresetSummary};
pub use model::{
    DegradedReason, DeviceId, DeviceInfo, EqState, LatencyMode, LayoutId, RoutingStatus, Stats,
};
pub use ports::{
    AudioBackend, BackendError, EndpointDirectory, Pipeline, PipelineConfig, PipelineError,
    PipelineFactory, VirtualDeviceProvider, VirtualError,
};
pub use presets::{Preset, PresetError, PresetStore};
pub use router::{Effect, Router, RouterInput};
pub use settings::{AppPaths, Settings, SettingsError, SettingsStore};
