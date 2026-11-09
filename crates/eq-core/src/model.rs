//! Domain types shared by the router, the controller, the ports and the UI.
//!
//! Types that are persisted (device ids, latency mode, EQ state) derive serde; the rest is
//! plain data passed between threads.

use std::fmt;

use eq_dsp::Layout;
use serde::{Deserialize, Serialize};

/// Backend-specific identifier of an audio endpoint.
///
/// On Windows this is the `MMDevice` endpoint id; on Linux the `PipeWire` `node.name`. The core
/// never interprets the string, it only compares and persists it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(String);

impl DeviceId {
    /// Wraps a backend-specific endpoint id.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// Returns the raw id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An audio output endpoint as reported by the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Backend-specific id, used to refer to the device in ports and events.
    pub id: DeviceId,
    /// Human-readable name shown to the user.
    pub name: String,
    /// `true` for the virtual device the app routes system audio into.
    pub is_virtual: bool,
}

/// Latency/reliability trade-off chosen by the user.
///
/// The mapping to buffer sizes lives in the backends; the core only passes the mode through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LatencyMode {
    /// Smallest buffers the device allows; more prone to underruns.
    Low,
    /// Default trade-off.
    #[default]
    Auto,
    /// Largest buffers; survives scheduling hiccups at the cost of delay.
    Reliable,
}

impl LatencyMode {
    /// All modes in UI order (slider detents from left to right).
    pub const ALL: [Self; 3] = [Self::Low, Self::Auto, Self::Reliable];

    /// Returns the label shown in the UI.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "Lower Latency",
            Self::Auto => "Automatic",
            Self::Reliable => "More Reliable",
        }
    }
}

/// Serde-friendly mirror of [`eq_dsp::Layout`].
///
/// `eq-dsp` deliberately has no serde dependency, so persisted state stores this type and
/// converts at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutId {
    /// The 10-band layout (32 Hz to 16 kHz).
    #[default]
    Ten,
}

impl From<LayoutId> for Layout {
    fn from(id: LayoutId) -> Self {
        match id {
            LayoutId::Ten => Self::Ten,
        }
    }
}

impl From<Layout> for LayoutId {
    fn from(layout: Layout) -> Self {
        match layout {
            Layout::Ten => Self::Ten,
        }
    }
}

/// The EQ state that is persisted with the settings and presets.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EqState {
    /// Band layout the gains refer to.
    pub layout: LayoutId,
    /// One gain in dB per band of `layout`.
    pub gains: Vec<f32>,
    /// Whether processing is applied (the power-off crossfade is handled by the DSP).
    pub enabled: bool,
}

/// Runtime statistics reported by a running pipeline.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// Measured end-to-end latency in milliseconds.
    pub latency_ms: f32,
    /// Number of buffer underruns since the pipeline started.
    pub underruns: u64,
    /// Number of drift resyncs since the pipeline started.
    pub resyncs: u64,
}

/// Why routing is degraded (power is requested but audio is not routed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradedReason {
    /// The virtual device could not be found or created.
    VirtualMissing,
    /// There is no output device to render to.
    NoTarget,
    /// The pipeline failed and could not be restarted.
    PipelineFailed,
}

/// Routing state as shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoutingStatus {
    /// Processing is off; the real device is the default output.
    #[default]
    Off,
    /// Audio is routed through the virtual device and the EQ.
    On,
    /// Power is on but routing is not working.
    Degraded(DegradedReason),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let json = serde_json::to_string(value).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn device_id_round_trips_as_plain_string() {
        let id = DeviceId::new("{0.0.0.00000000}.{abc}");
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, "\"{0.0.0.00000000}.{abc}\"");
        assert_eq!(round_trip(&id), id);
    }

    #[test]
    fn latency_mode_round_trips_in_lowercase() {
        for mode in LatencyMode::ALL {
            assert_eq!(round_trip(&mode), mode);
        }
        let json = serde_json::to_string(&LatencyMode::Reliable).expect("serialize");
        assert_eq!(json, "\"reliable\"");
    }

    #[test]
    fn latency_mode_defaults_to_automatic() {
        assert_eq!(LatencyMode::default(), LatencyMode::Auto);
        assert_eq!(
            LatencyMode::ALL.map(LatencyMode::label),
            ["Lower Latency", "Automatic", "More Reliable",]
        );
    }

    #[test]
    fn eq_state_round_trips() {
        let state = EqState {
            layout: LayoutId::Ten,
            gains: vec![0.0, 1.5, -3.0, 12.0, -12.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            enabled: true,
        };
        assert_eq!(round_trip(&state), state);
    }

    #[test]
    fn layout_id_converts_to_and_from_dsp_layout() {
        let layout: Layout = LayoutId::Ten.into();
        assert_eq!(LayoutId::from(layout), LayoutId::Ten);
    }

    #[test]
    fn device_info_is_plain_data() {
        let info = DeviceInfo {
            id: DeviceId::new("node"),
            name: "Speakers".to_owned(),
            is_virtual: false,
        };
        assert_eq!(info.clone(), info);
        assert_eq!(DeviceId::new("node").to_string(), "node");
    }
}
