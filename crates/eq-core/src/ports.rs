//! Ports: the traits that the OS backends implement, and the errors they return.
//!
//! `eq-core` never names a platform type. Each backend crate provides one [`AudioBackend`]
//! (an Abstract Factory) that hands out the adapters for its platform; tests provide fakes.

use eq_dsp::EqParams;
use thiserror::Error;

use crate::model::{DeviceId, DeviceInfo, LatencyMode, Stats};

/// Abstract Factory for one platform's audio adapters.
///
/// Only the composition root (`eq-app`) names a concrete implementation. The accessors return
/// borrowed adapters so the backend keeps ownership of its OS resources (COM apartment,
/// `PipeWire` loop) for its whole lifetime. Backends are created and used on the controller
/// thread only; they need not be `Send`.
pub trait AudioBackend {
    /// Returns the endpoint directory (device listing and default-device control).
    fn directory(&self) -> &dyn EndpointDirectory;

    /// Returns the virtual device provider.
    ///
    /// Mutable because `ensure` and `release` change OS state the provider owns (the Linux
    /// null sink exists only while the provider holds it).
    fn virtual_provider(&mut self) -> &mut dyn VirtualDeviceProvider;

    /// Returns the factory that builds real-time pipelines.
    fn pipeline_factory(&self) -> &dyn PipelineFactory;
}

/// Lists output endpoints and changes the system default output.
pub trait EndpointDirectory {
    /// Lists the active render (output) endpoints, including the virtual device.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the OS enumeration fails.
    fn list_render(&self) -> Result<Vec<DeviceInfo>, BackendError>;

    /// Returns the current default output, or `None` when no output device exists.
    ///
    /// `None` is a valid state (for example, every output was unplugged), so it is not an
    /// error.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the OS query fails.
    fn default_render(&self) -> Result<Option<DeviceId>, BackendError>;

    /// Makes `id` the default output for every role the platform uses.
    ///
    /// # Errors
    ///
    /// Returns [`BackendError`] when the device does not exist or the OS rejects the change.
    fn set_default_render(&self, id: &DeviceId) -> Result<(), BackendError>;
}

/// Provides the virtual device that system audio is routed into.
///
/// Windows locates the installed VB-CABLE endpoint; Linux creates and destroys a null sink.
pub trait VirtualDeviceProvider {
    /// Makes the virtual device available and returns its id.
    ///
    /// `devices` is the current device list, so an implementation can locate an existing
    /// endpoint without another enumeration.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualError::NotFound`] when a pre-installed device is missing, or another
    /// [`VirtualError`] when creating it fails.
    fn ensure(&mut self, devices: &[DeviceInfo]) -> Result<DeviceId, VirtualError>;

    /// Releases the virtual device. A no-op for providers that only locate an endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualError`] when the device cannot be destroyed.
    fn release(&mut self) -> Result<(), VirtualError>;
}

/// Builds the real-time pipeline (capture from the virtual device, EQ, render to the target).
pub trait PipelineFactory {
    /// Starts a pipeline for `config`.
    ///
    /// # Errors
    ///
    /// Returns [`PipelineError`] when a stream cannot be opened or a device is missing.
    fn start(&self, config: PipelineConfig) -> Result<Box<dyn Pipeline>, PipelineError>;
}

/// A running pipeline, owned by the controller.
///
/// `Send` because the controller thread owns it. Implementations must not block in
/// `set_eq` or `stats`: the controller calls them from its event loop.
pub trait Pipeline: Send {
    /// Replaces the EQ parameters. The audio thread picks them up at its next period.
    fn set_eq(&mut self, eq: EqParams);

    /// Returns the latest statistics. Reads atomics only.
    fn stats(&self) -> Stats;

    /// Stops the pipeline and releases its OS resources.
    ///
    /// Takes the box by value so the pipeline cannot be used after it stops.
    fn stop(self: Box<Self>);
}

/// Parameters for [`PipelineFactory::start`].
#[derive(Debug, Clone, PartialEq)]
pub struct PipelineConfig {
    /// Virtual device to capture from.
    pub virtual_id: DeviceId,
    /// Real output device to render to.
    pub target_id: DeviceId,
    /// Latency mode, which sets the period and the FIFO target.
    pub latency: LatencyMode,
    /// Initial EQ parameters.
    pub eq: EqParams,
}

/// Errors reported by [`AudioBackend`] adapters.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BackendError {
    /// The OS audio service is not running or not reachable (for example, `PipeWire` is down).
    #[error("audio backend unavailable: {0}")]
    Unavailable(String),
    /// An operation was rejected or failed.
    #[error("audio backend operation failed: {0}")]
    Failed(String),
}

/// Errors reported by [`VirtualDeviceProvider`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VirtualError {
    /// The pre-installed virtual device was not found (name pattern did not match).
    #[error("virtual device not found (pattern `{0}`)")]
    NotFound(String),
    /// The underlying OS query failed.
    #[error(transparent)]
    Backend(#[from] BackendError),
    /// Creating or destroying the virtual device failed.
    #[error("virtual device operation failed: {0}")]
    Failed(String),
}

/// Errors reported by [`PipelineFactory`] and [`Pipeline`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PipelineError {
    /// A device named in the configuration no longer exists.
    #[error("pipeline device missing: {0}")]
    DeviceMissing(DeviceId),
    /// The stream failed to start or stopped unexpectedly.
    #[error("pipeline failed: {0}")]
    Failed(String),
}
