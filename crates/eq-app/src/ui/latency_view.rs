//! Helpers of the latency controls: slider detents, the measured-latency label and the dropout
//! hint.
//!
//! Pure functions and a small state machine, so the rules are covered by unit tests.

use eq_core::LatencyMode;

/// Label shown instead of a latency value while audio is not routed.
pub const NOT_ROUTING_LABEL: &str = "\u{2014}";

/// Detent index of a latency mode on the slider: 0 is the leftmost detent.
#[must_use]
pub const fn mode_index(mode: LatencyMode) -> i32 {
    match mode {
        LatencyMode::Low => 0,
        LatencyMode::Auto => 1,
        LatencyMode::Reliable => 2,
    }
}

/// Latency mode of a detent index, or `None` when the index is out of range.
#[must_use]
pub fn mode_from_index(index: i32) -> Option<LatencyMode> {
    usize::try_from(index)
        .ok()
        .and_then(|index| LatencyMode::ALL.get(index).copied())
}

/// Text of the measured latency: `40 ms`, or `—` while audio is not routed (`None`).
#[must_use]
pub fn latency_text(latency_ms: Option<f32>) -> String {
    latency_ms.map_or_else(
        || NOT_ROUTING_LABEL.to_owned(),
        |ms| format!("{:.0} ms", ms.max(0.0)),
    )
}

/// Decides when the "Dropouts detected — try Automatic." hint is shown.
///
/// The hint appears once the underrun counter has risen in [`Self::STREAK_LIMIT`] consecutive
/// statistics events while the mode is [`LatencyMode::Low`]. It then stays visible until the
/// mode changes, so that it does not flicker when the counter stops rising.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnderrunMonitor {
    /// Mode the counts belong to. `None` until the first mode is set.
    mode: Option<LatencyMode>,
    /// Counter value of the previous event in the current mode.
    last_underruns: Option<u64>,
    /// Number of consecutive events in which the counter rose.
    rising_streak: u32,
    /// Whether the hint is shown.
    hint: bool,
}

impl UnderrunMonitor {
    /// Consecutive rising events needed before the hint appears.
    pub const STREAK_LIMIT: u32 = 3;

    /// Sets the current latency mode. A different mode clears all history and the hint.
    pub fn set_mode(&mut self, mode: LatencyMode) {
        if self.mode != Some(mode) {
            *self = Self {
                mode: Some(mode),
                ..Self::default()
            };
        }
    }

    /// Feeds the underrun counter of one statistics event and returns whether the hint is shown.
    ///
    /// The first value after a mode change only sets the baseline; it is not counted as a rise.
    /// Outside [`LatencyMode::Low`] the hint is never shown.
    pub fn record(&mut self, underruns: u64) -> bool {
        if self.mode != Some(LatencyMode::Low) {
            return false;
        }
        let rose = self.last_underruns.is_some_and(|last| underruns > last);
        self.last_underruns = Some(underruns);
        self.rising_streak = if rose {
            self.rising_streak.saturating_add(1)
        } else {
            0
        };
        if self.rising_streak >= Self::STREAK_LIMIT {
            self.hint = true;
        }
        self.hint
    }

    /// Whether the hint is currently shown.
    #[must_use]
    pub const fn hint(&self) -> bool {
        self.hint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detents_follow_the_slider_order() {
        assert_eq!(mode_index(LatencyMode::Low), 0);
        assert_eq!(mode_index(LatencyMode::Auto), 1);
        assert_eq!(mode_index(LatencyMode::Reliable), 2);
        for (index, mode) in LatencyMode::ALL.into_iter().enumerate() {
            let index = i32::try_from(index).expect("small index");
            assert_eq!(mode_index(mode), index);
            assert_eq!(mode_from_index(index), Some(mode));
        }
    }

    #[test]
    fn out_of_range_detents_map_to_no_mode() {
        assert_eq!(mode_from_index(-1), None);
        assert_eq!(mode_from_index(3), None);
    }

    #[test]
    fn latency_is_rounded_to_whole_milliseconds() {
        assert_eq!(latency_text(Some(40.0)), "40 ms");
        assert_eq!(latency_text(Some(40.4)), "40 ms");
        assert_eq!(latency_text(Some(40.6)), "41 ms");
        assert_eq!(latency_text(Some(-0.2)), "0 ms");
    }

    #[test]
    fn latency_shows_a_dash_when_not_routing() {
        assert_eq!(latency_text(None), "\u{2014}");
    }

    #[test]
    fn three_rising_events_in_lower_latency_show_the_hint() {
        let mut monitor = UnderrunMonitor::default();
        monitor.set_mode(LatencyMode::Low);
        // The first value is the baseline; the three rises after it are the streak.
        assert!(!monitor.record(0));
        assert!(!monitor.record(1));
        assert!(!monitor.record(2));
        assert!(monitor.record(3));
    }

    #[test]
    fn a_flat_event_breaks_the_streak() {
        let mut monitor = UnderrunMonitor::default();
        monitor.set_mode(LatencyMode::Low);
        monitor.record(0);
        monitor.record(1);
        monitor.record(2);
        monitor.record(2);
        assert!(!monitor.record(3));
        assert!(!monitor.hint());
    }

    #[test]
    fn the_hint_stays_until_the_mode_changes() {
        let mut monitor = UnderrunMonitor::default();
        monitor.set_mode(LatencyMode::Low);
        monitor.record(0);
        monitor.record(1);
        monitor.record(2);
        assert!(monitor.record(3));
        assert!(monitor.record(3));
        monitor.set_mode(LatencyMode::Auto);
        assert!(!monitor.hint());
        assert!(!monitor.record(9));
    }

    #[test]
    fn switching_back_to_lower_latency_starts_a_fresh_count() {
        let mut monitor = UnderrunMonitor::default();
        monitor.set_mode(LatencyMode::Low);
        monitor.record(0);
        monitor.record(1);
        monitor.record(2);
        monitor.record(3);
        monitor.set_mode(LatencyMode::Auto);
        monitor.set_mode(LatencyMode::Low);
        // The counter is cumulative, so its current value is only a baseline.
        assert!(!monitor.record(3));
        assert!(!monitor.record(4));
        assert!(!monitor.record(5));
        assert!(monitor.record(6));
    }

    #[test]
    fn other_modes_never_show_the_hint() {
        let mut monitor = UnderrunMonitor::default();
        monitor.set_mode(LatencyMode::Auto);
        for underruns in 0..10 {
            assert!(!monitor.record(underruns));
        }
        monitor.set_mode(LatencyMode::Reliable);
        assert!(!monitor.record(100));
    }

    #[test]
    fn without_a_mode_nothing_is_recorded() {
        let mut monitor = UnderrunMonitor::default();
        assert!(!monitor.record(5));
        assert!(!monitor.hint());
    }

    #[test]
    fn setting_the_same_mode_keeps_the_history() {
        let mut monitor = UnderrunMonitor::default();
        monitor.set_mode(LatencyMode::Low);
        monitor.record(0);
        monitor.record(1);
        monitor.record(2);
        monitor.set_mode(LatencyMode::Low);
        assert!(monitor.record(3));
    }
}
