//! Conversions from controller state to the text and flags the UI shows.
//!
//! Pure functions only, so the wording and formatting rules are covered by unit tests.

use eq_core::{AppSnapshot, DegradedReason, RoutingStatus};

/// Formats a band gain as the value label under a slider: `+3.0 dB`, `0.0 dB`, `-2.5 dB`.
#[must_use]
pub fn format_gain(db: f32) -> String {
    // Round to the displayed precision first so that -0.04 reads as 0.0 instead of -0.0.
    let tenths = (db * 10.0).round() / 10.0;
    if tenths.abs() < 0.05 {
        "0.0 dB".to_owned()
    } else {
        format!("{tenths:+.1} dB")
    }
}

/// Value labels for all band gains, in band order.
#[must_use]
pub fn gain_labels(gains: &[f32]) -> Vec<String> {
    gains.iter().map(|&db| format_gain(db)).collect()
}

/// `true` when the routing state is degraded and should be shown as a problem.
#[must_use]
pub const fn status_is_degraded(status: RoutingStatus) -> bool {
    matches!(status, RoutingStatus::Degraded(_))
}

/// Status line under the app name, for the current platform.
#[must_use]
pub fn status_text(snapshot: &AppSnapshot) -> String {
    status_text_for(snapshot, cfg!(windows))
}

/// Status line under the output dropdown. The output itself is shown by the dropdown, so this
/// line only says whether routing is on, or why it is not. `windows` selects the
/// platform-specific hint for a missing virtual device (VB-CABLE on Windows, `PipeWire` on
/// Linux).
#[must_use]
pub fn status_text_for(snapshot: &AppSnapshot, windows: bool) -> String {
    match snapshot.status {
        RoutingStatus::Off => "Off".to_owned(),
        RoutingStatus::On => "On".to_owned(),
        RoutingStatus::Degraded(reason) => degraded_text(reason, windows).to_owned(),
    }
}

/// Hint shown when routing is degraded.
#[must_use]
pub const fn degraded_text(reason: DegradedReason, windows: bool) -> &'static str {
    match reason {
        DegradedReason::VirtualMissing => {
            if windows {
                "Virtual device not found \u{2014} install VB-CABLE"
            } else {
                "Virtual device unavailable \u{2014} PipeWire not running?"
            }
        }
        DegradedReason::NoTarget => "No output device available",
        DegradedReason::PipelineFailed => "Audio pipeline failed \u{2014} check the output device",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(status: RoutingStatus, target: Option<&str>) -> AppSnapshot {
        AppSnapshot {
            status,
            target_name: target.map(str::to_owned),
            ..AppSnapshot::default()
        }
    }

    #[test]
    fn gains_are_formatted_with_sign_and_one_decimal() {
        assert_eq!(format_gain(3.0), "+3.0 dB");
        assert_eq!(format_gain(12.0), "+12.0 dB");
        assert_eq!(format_gain(-2.5), "-2.5 dB");
        assert_eq!(format_gain(-12.0), "-12.0 dB");
        assert_eq!(format_gain(0.0), "0.0 dB");
    }

    #[test]
    fn gains_near_zero_never_show_negative_zero() {
        assert_eq!(format_gain(-0.0), "0.0 dB");
        assert_eq!(format_gain(-0.04), "0.0 dB");
        assert_eq!(format_gain(0.04), "0.0 dB");
    }

    #[test]
    fn gains_round_to_the_displayed_tenth() {
        assert_eq!(format_gain(1.26), "+1.3 dB");
        assert_eq!(format_gain(-0.6), "-0.6 dB");
    }

    #[test]
    fn gain_labels_follow_band_order() {
        assert_eq!(
            gain_labels(&[0.0, 1.5, -3.0]),
            vec!["0.0 dB", "+1.5 dB", "-3.0 dB"]
        );
    }

    #[test]
    fn only_degraded_status_is_flagged() {
        assert!(!status_is_degraded(RoutingStatus::Off));
        assert!(!status_is_degraded(RoutingStatus::On));
        assert!(status_is_degraded(RoutingStatus::Degraded(
            DegradedReason::NoTarget
        )));
    }

    #[test]
    fn on_and_off_status_do_not_repeat_the_output_name() {
        let on = snapshot(RoutingStatus::On, Some("Demo output"));
        assert_eq!(status_text_for(&on, true), "On");
        assert_eq!(status_text_for(&on, false), "On");
        let off = snapshot(RoutingStatus::Off, Some("Speakers"));
        assert_eq!(status_text_for(&off, true), "Off");
        let without = snapshot(RoutingStatus::Off, None);
        assert_eq!(status_text_for(&without, true), "Off");
    }

    #[test]
    fn every_degraded_reason_has_a_hint() {
        assert_eq!(
            degraded_text(DegradedReason::VirtualMissing, true),
            "Virtual device not found \u{2014} install VB-CABLE"
        );
        assert_eq!(
            degraded_text(DegradedReason::VirtualMissing, false),
            "Virtual device unavailable \u{2014} PipeWire not running?"
        );
        assert_eq!(
            degraded_text(DegradedReason::NoTarget, true),
            "No output device available"
        );
        assert_eq!(
            degraded_text(DegradedReason::NoTarget, false),
            "No output device available"
        );
        assert_eq!(
            degraded_text(DegradedReason::PipelineFailed, true),
            "Audio pipeline failed \u{2014} check the output device"
        );
    }

    #[test]
    fn degraded_status_uses_the_reason_hint() {
        let s = snapshot(
            RoutingStatus::Degraded(DegradedReason::NoTarget),
            Some("Speakers"),
        );
        assert_eq!(status_text_for(&s, true), "No output device available");
    }
}
