//! Windows backend for Equalizerer: COM, `MMDevice` device enumeration, `IPolicyConfig` for
//! setting the default endpoint, device notifications, and the WASAPI pipeline.
//!
//! The crate is empty on other operating systems.
//!
//! # Threading
//!
//! COM objects in this crate are bound to the thread that created them. The backend is created
//! and used only on the controller thread, inside a [`ComApartment`] that lives as long as the
//! objects. Notification callbacks run on COM worker threads; they only convert the
//! notification and send it through a channel.

#![cfg(windows)]

pub mod autostart;
pub mod backend;
pub mod com;
pub mod devices;
pub mod error;
mod format;
pub mod instance;
pub mod notifications;
pub mod pipeline;
pub mod policy_config;
pub mod restore;
pub mod virtual_device;

pub use autostart::{
    disable as disable_autostart, enable as enable_autostart, is_enabled as is_autostart_enabled,
};
pub use backend::WasapiBackend;
pub use com::ComApartment;
pub use devices::MmDirectory;
pub use error::WasapiError;
pub use instance::SingleInstance;
pub use notifications::NotificationGuard;
pub use pipeline::WasapiPipelineFactory;
pub use policy_config::RestoreDefaultGuard;
pub use restore::restore_default_device;
pub use virtual_device::CableProvider;
