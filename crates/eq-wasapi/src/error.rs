//! Error type of the Windows backend and its mapping into the `eq-core` port errors.

use eq_core::{BackendError, VirtualError};
use thiserror::Error;
use windows::core::HRESULT;

/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`, returned by `GetDefaultAudioEndpoint` when no
/// endpoint of the requested data flow exists, and by `IPolicyConfig` for unknown device ids.
pub(crate) const HRESULT_NOT_FOUND: HRESULT = HRESULT(-2_147_023_728); // 0x8007_0490

/// Errors reported by the Windows backend.
#[derive(Debug, Error)]
pub enum WasapiError {
    /// A COM or Windows API call failed. `context` names the call that failed.
    #[error("{context} failed: {source}")]
    Com {
        /// The API call that failed, for example `IMMDevice::GetId`.
        context: &'static str,
        /// The underlying Windows error.
        source: windows::core::Error,
    },
    /// No output device matches the given id or name.
    #[error("no output device matches `{0}`")]
    DeviceNotFound(String),
    /// The device mix format is not the 32-bit float format the pipeline needs.
    #[error("unsupported device format: {0}")]
    UnsupportedFormat(String),
    /// The sample processor could not be built for the device format.
    #[error("audio processor setup failed: {0}")]
    Processor(#[from] eq_engine::ProcessorError),
}

/// Attaches a call name to a [`windows::core::Result`], producing a [`WasapiError::Com`].
pub trait ComResultExt<T> {
    /// Converts the error into [`WasapiError::Com`] tagged with `context`.
    ///
    /// # Errors
    ///
    /// Returns [`WasapiError::Com`] when the result is an error.
    fn context(self, context: &'static str) -> Result<T, WasapiError>;
}

impl<T> ComResultExt<T> for windows::core::Result<T> {
    fn context(self, context: &'static str) -> Result<T, WasapiError> {
        self.map_err(|source| WasapiError::Com { context, source })
    }
}

impl From<WasapiError> for BackendError {
    /// Maps every backend failure to [`BackendError::Failed`] with the full message.
    fn from(err: WasapiError) -> Self {
        Self::Failed(err.to_string())
    }
}

impl From<WasapiError> for VirtualError {
    fn from(err: WasapiError) -> Self {
        Self::Backend(err.into())
    }
}
