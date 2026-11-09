//! Messages exchanged between threads.
//!
//! Threads communicate only through these messages (no shared mutable state): backends send
//! [`DeviceEvent`]s to the controller, the UI sends [`AppCommand`]s, and the controller answers
//! with [`AppEvent`]s.

use crate::model::{DeviceId, DeviceInfo, LatencyMode, RoutingStatus, Stats};

/// Something that happened in the OS or in a pipeline, reported to the controller.
///
/// Backends only forward these; the controller decides what they mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceEvent {
    /// The system default output changed to the given device.
    DefaultChanged(DeviceId),
    /// An output device appeared.
    Added(DeviceId),
    /// An output device disappeared.
    Removed(DeviceId),
    /// A running pipeline stopped with the given reason.
    PipelineFailed(String),
}

/// A request from the UI to the controller.
#[derive(Debug, Clone, PartialEq)]
pub enum AppCommand {
    /// Turns processing and routing on or off.
    SetPower(bool),
    /// Sets one band's gain in dB.
    SetGain {
        /// Zero-based band index within the current layout.
        band: usize,
        /// New gain in dB.
        db: f32,
    },
    /// Replaces all band gains at once (preset selection, reset).
    SetGains(Vec<f32>),
    /// Loads the named preset (built-in or user).
    SelectPreset(String),
    /// Overwrites the active user preset with the current gains.
    SavePreset,
    /// Creates a user preset with the given name from the current gains.
    SavePresetAs(String),
    /// Renames a user preset.
    RenamePreset {
        /// Current name.
        from: String,
        /// New name.
        to: String,
    },
    /// Deletes the named user preset.
    DeletePreset(String),
    /// Changes the latency mode, which restarts the pipeline.
    SetLatency(LatencyMode),
    /// Chooses the real output device to equalize. Ignored for the virtual device and for ids
    /// the controller does not know.
    SelectTarget(DeviceId),
    /// Asks the running instance to show its window.
    ShowWindow,
    /// Powers off (restoring the real default) and exits.
    Quit,
}

/// A message from the controller to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEvent {
    /// Full state, sent after every change the UI must display.
    Snapshot(AppSnapshot),
    /// Periodic pipeline statistics (about four times per second while routing).
    Stats(Stats),
    /// A user-visible error message (for example, a preset name that is not allowed).
    Error(String),
}

/// Everything the UI needs to render the main window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppSnapshot {
    /// Whether the power button is on.
    pub power: bool,
    /// Routing state, including why it is degraded.
    pub status: RoutingStatus,
    /// Name of the target output device, if one is selected.
    pub target_name: Option<String>,
    /// Id of the target output device, if one is selected.
    pub target_id: Option<DeviceId>,
    /// The real output devices the user can choose from. The virtual device is never listed.
    pub devices: Vec<DeviceInfo>,
    /// All presets, built-in first.
    pub presets: Vec<PresetSummary>,
    /// Name of the preset the gains were loaded from, if any.
    pub active_preset: Option<String>,
    /// `true` when the gains differ from the active preset.
    pub modified: bool,
    /// Current band gains in dB.
    pub gains: Vec<f32>,
    /// Selected latency mode.
    pub latency: LatencyMode,
}

/// Name and origin of a preset, for the preset list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetSummary {
    /// Preset name, unique case-insensitively.
    pub name: String,
    /// `true` for read-only built-in presets.
    pub builtin: bool,
}
