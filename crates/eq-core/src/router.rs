//! Routing state machine: decides which device is the target, which device is the default,
//! and when the pipeline runs.
//!
//! The router is pure. It receives [`RouterInput`]s and returns [`Effect`]s; it performs no
//! I/O, starts no threads and reads no clock. The controller executes the effects through the
//! ports and feeds the answers back as inputs (for example, [`RouterInput::VirtualReady`] after
//! [`Effect::EnsureVirtual`]). The transition table is `docs/architecture.md` section 7.

use crate::model::{DegradedReason, DeviceId, DeviceInfo, RoutingStatus};

/// How many previous targets are remembered as fallbacks.
const HISTORY_LEN: usize = 4;

/// An event the router reacts to.
///
/// Compared with the table in `architecture.md` section 7, the set has two additions that let
/// the controller report asynchronous results without the router doing I/O:
/// [`RouterInput::VirtualFailed`] answers `EnsureVirtual` with a failure, and
/// [`RouterInput::PipelineRecovered`] reports that a retry after [`RouterInput::PipelineFailed`]
/// succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouterInput {
    /// The user turned power on. `current_default` and `devices` are the OS state at that time.
    PowerOn {
        /// The system default output when power was turned on, if any.
        current_default: Option<DeviceId>,
        /// The active output devices, including the virtual one if it already exists.
        devices: Vec<DeviceInfo>,
    },
    /// The user turned power off, or the app is shutting down.
    PowerOff,
    /// `EnsureVirtual` succeeded; the virtual device has this id.
    VirtualReady(DeviceId),
    /// `EnsureVirtual` failed.
    VirtualFailed,
    /// The system default output changed to this device (possibly our own virtual device).
    DefaultChanged(DeviceId),
    /// An output device disappeared.
    DeviceRemoved(DeviceId),
    /// An output device appeared, or its description changed.
    DeviceAdded(DeviceInfo),
    /// The running pipeline stopped unexpectedly.
    PipelineFailed,
    /// A controller-side retry of the pipeline succeeded.
    PipelineRecovered,
}

/// A side effect the controller must perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Make the virtual device available (Windows: locate it; Linux: create the sink).
    /// The controller answers with [`RouterInput::VirtualReady`] or [`RouterInput::VirtualFailed`].
    EnsureVirtual,
    /// Release the virtual device (Linux: destroy the sink; Windows: no-op).
    ReleaseVirtual,
    /// Make this device the default output for every role.
    SetDefault(DeviceId),
    /// Start a pipeline that captures from `virtual_id` and renders to `target_id`.
    StartPipeline {
        /// Device to capture from.
        virtual_id: DeviceId,
        /// Device to render to.
        target_id: DeviceId,
    },
    /// Stop the running pipeline, if any.
    StopPipeline,
    /// Remember this device as the target for the next session.
    PersistTarget(DeviceId),
    /// Report a new routing status to the UI.
    Status(RoutingStatus),
}

/// Internal lifecycle phase. [`RoutingStatus`] is derived from it, except that `Ensuring`
/// (waiting for the virtual device) reports as [`RoutingStatus::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Power is off.
    Off,
    /// `EnsureVirtual` was issued and its answer is pending.
    Ensuring,
    /// Audio is routed through the virtual device into the target.
    On,
    /// Power is on but routing is not working.
    Degraded(DegradedReason),
}

/// The routing state machine.
///
/// Create one with [`Router::new`], feed it inputs with [`Router::handle`], and read the state
/// with [`Router::status`] and [`Router::target`].
#[derive(Debug, Clone)]
pub struct Router {
    phase: Phase,
    /// Last known device list, used to choose fallback targets.
    devices: Vec<DeviceInfo>,
    /// Id of the virtual device, known once `VirtualReady` arrived.
    virtual_id: Option<DeviceId>,
    /// Current target, or the last known real default while routing is off.
    target: Option<DeviceId>,
    /// Previous targets, most recent first, without duplicates and without `target`.
    history: Vec<DeviceId>,
    /// The system default when power was turned on; consumed by `VirtualReady`.
    default_at_power_on: Option<DeviceId>,
}

impl Router {
    /// Creates a router in the `Off` state.
    ///
    /// `persisted_target` is the target saved by the previous session. It is used as the first
    /// fallback when power is turned on while the default is already the virtual device.
    #[must_use]
    pub const fn new(persisted_target: Option<DeviceId>) -> Self {
        Self {
            phase: Phase::Off,
            devices: Vec::new(),
            virtual_id: None,
            target: persisted_target,
            history: Vec::new(),
            default_at_power_on: None,
        }
    }

    /// Returns the routing status to show in the UI.
    #[must_use]
    pub const fn status(&self) -> RoutingStatus {
        match self.phase {
            Phase::Off | Phase::Ensuring => RoutingStatus::Off,
            Phase::On => RoutingStatus::On,
            Phase::Degraded(reason) => RoutingStatus::Degraded(reason),
        }
    }

    /// Returns the current target device, or the candidate remembered while routing is off.
    #[must_use]
    pub const fn target(&self) -> Option<&DeviceId> {
        self.target.as_ref()
    }

    /// Applies one input and returns the effects the controller must perform, in order.
    #[must_use]
    pub fn handle(&mut self, input: RouterInput) -> Vec<Effect> {
        match input {
            RouterInput::PowerOn {
                current_default,
                devices,
            } => self.power_on(current_default, devices),
            RouterInput::PowerOff => self.power_off(),
            RouterInput::VirtualReady(id) => self.virtual_ready(id),
            RouterInput::VirtualFailed => self.virtual_failed(),
            RouterInput::DefaultChanged(id) => self.default_changed(id),
            RouterInput::DeviceRemoved(id) => self.device_removed(&id),
            RouterInput::DeviceAdded(info) => self.device_added(info),
            RouterInput::PipelineFailed => self.pipeline_failed(),
            RouterInput::PipelineRecovered => self.pipeline_recovered(),
        }
    }

    /// `PowerOn` is accepted only when off; a second power-on is ignored.
    fn power_on(
        &mut self,
        current_default: Option<DeviceId>,
        devices: Vec<DeviceInfo>,
    ) -> Vec<Effect> {
        if self.phase != Phase::Off {
            return Vec::new();
        }
        self.devices = devices;
        self.virtual_id = None;
        self.phase = Phase::Ensuring;
        // A real default is the best candidate for the target and is what a failed power-on
        // must restore, so it is remembered even before the virtual device is ready.
        let real_default = current_default.clone().filter(|id| !self.is_virtual(id));
        if let Some(default) = real_default {
            self.set_target(Some(default));
        }
        self.default_at_power_on = current_default;
        vec![Effect::EnsureVirtual]
    }

    /// Restores the real default and releases the virtual device. Ignored when already off.
    fn power_off(&mut self) -> Vec<Effect> {
        if self.phase == Phase::Off {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if self.pipeline_active() {
            effects.push(Effect::StopPipeline);
        }
        if let Some(target) = self.target.clone() {
            effects.push(Effect::SetDefault(target));
        }
        effects.push(Effect::ReleaseVirtual);
        effects.push(Effect::Status(RoutingStatus::Off));

        self.phase = Phase::Off;
        self.virtual_id = None;
        self.default_at_power_on = None;
        effects
    }

    /// Completes power-on: picks the target and starts routing, or degrades if there is none.
    fn virtual_ready(&mut self, virtual_id: DeviceId) -> Vec<Effect> {
        if self.phase != Phase::Ensuring {
            return Vec::new();
        }
        self.virtual_id = Some(virtual_id.clone());
        if let Some(target) = self.initial_target(&virtual_id) {
            self.set_target(Some(target.clone()));
            self.phase = Phase::On;
            vec![
                Effect::SetDefault(virtual_id.clone()),
                Effect::StartPipeline {
                    virtual_id,
                    target_id: target.clone(),
                },
                Effect::PersistTarget(target),
                Effect::Status(RoutingStatus::On),
            ]
        } else {
            self.set_target(None);
            self.phase = Phase::Degraded(DegradedReason::NoTarget);
            vec![Effect::Status(RoutingStatus::Degraded(
                DegradedReason::NoTarget,
            ))]
        }
    }

    /// `EnsureVirtual` failed; power stays on but routing is degraded.
    fn virtual_failed(&mut self) -> Vec<Effect> {
        if self.phase != Phase::Ensuring {
            return Vec::new();
        }
        self.phase = Phase::Degraded(DegradedReason::VirtualMissing);
        vec![Effect::Status(RoutingStatus::Degraded(
            DegradedReason::VirtualMissing,
        ))]
    }

    /// Handles a default-device change, which is either our own change or the user's choice.
    fn default_changed(&mut self, id: DeviceId) -> Vec<Effect> {
        // Our own switch to the virtual device comes back as this event; ignore it.
        if self.is_virtual(&id) {
            return Vec::new();
        }
        match self.phase {
            Phase::Off | Phase::Degraded(DegradedReason::VirtualMissing) => {
                self.set_target(Some(id));
                Vec::new()
            }
            Phase::Ensuring => {
                self.default_at_power_on = Some(id);
                Vec::new()
            }
            Phase::On if self.target.as_ref() == Some(&id) => Vec::new(),
            Phase::On
            | Phase::Degraded(DegradedReason::NoTarget | DegradedReason::PipelineFailed) => {
                self.restart_on(id)
            }
        }
    }

    /// Handles the removal of a device: the virtual device degrades routing, the target is
    /// replaced by a fallback.
    fn device_removed(&mut self, id: &DeviceId) -> Vec<Effect> {
        let was_virtual = self.is_virtual(id);
        let was_target = self.target.as_ref() == Some(id);
        self.devices.retain(|device| device.id != *id);
        self.history.retain(|old| old != id);

        if was_virtual {
            return self.virtual_removed();
        }
        if !was_target {
            return Vec::new();
        }

        // The removed device must not come back as a fallback, so it is dropped without
        // being remembered in the history.
        self.target = None;
        match self.phase {
            Phase::On | Phase::Degraded(DegradedReason::PipelineFailed) => {
                let mut effects = vec![Effect::StopPipeline];
                if let Some(next) = self.fallback() {
                    self.set_target(Some(next.clone()));
                    self.phase = Phase::On;
                    if let Some(virtual_id) = self.virtual_id.clone() {
                        effects.push(Effect::StartPipeline {
                            virtual_id: virtual_id.clone(),
                            target_id: next.clone(),
                        });
                        effects.push(Effect::SetDefault(virtual_id));
                    }
                    effects.push(Effect::PersistTarget(next));
                    effects.push(Effect::Status(RoutingStatus::On));
                } else {
                    self.phase = Phase::Degraded(DegradedReason::NoTarget);
                    effects.push(Effect::Status(RoutingStatus::Degraded(
                        DegradedReason::NoTarget,
                    )));
                }
                effects
            }
            _ => {
                // Not routing: only the candidate changes, nothing is started.
                self.target = self.fallback();
                Vec::new()
            }
        }
    }

    /// The virtual device disappeared while routing; the pipeline cannot run without it.
    fn virtual_removed(&mut self) -> Vec<Effect> {
        if !matches!(
            self.phase,
            Phase::On | Phase::Degraded(DegradedReason::NoTarget | DegradedReason::PipelineFailed)
        ) {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if self.pipeline_active() {
            effects.push(Effect::StopPipeline);
        }
        self.virtual_id = None;
        self.phase = Phase::Degraded(DegradedReason::VirtualMissing);
        effects.push(Effect::Status(RoutingStatus::Degraded(
            DegradedReason::VirtualMissing,
        )));
        effects
    }

    /// Records a new device. A new real device resolves `NoTarget` degradation.
    fn device_added(&mut self, info: DeviceInfo) -> Vec<Effect> {
        let id = info.id.clone();
        let is_real = !info.is_virtual;
        if let Some(existing) = self.devices.iter_mut().find(|device| device.id == id) {
            *existing = info;
        } else {
            self.devices.push(info);
        }
        if is_real && self.phase == Phase::Degraded(DegradedReason::NoTarget) {
            return self.restart_on(id);
        }
        Vec::new()
    }

    /// The pipeline stopped unexpectedly; the controller decides how to retry.
    fn pipeline_failed(&mut self) -> Vec<Effect> {
        if self.phase != Phase::On {
            return Vec::new();
        }
        self.phase = Phase::Degraded(DegradedReason::PipelineFailed);
        vec![Effect::Status(RoutingStatus::Degraded(
            DegradedReason::PipelineFailed,
        ))]
    }

    /// A retry succeeded; routing is back on.
    fn pipeline_recovered(&mut self) -> Vec<Effect> {
        if self.phase != Phase::Degraded(DegradedReason::PipelineFailed) {
            return Vec::new();
        }
        self.phase = Phase::On;
        vec![Effect::Status(RoutingStatus::On)]
    }

    /// Makes `id` the new target and (re)starts routing into it.
    ///
    /// Effect order: `StopPipeline` (only if one runs), `StartPipeline`, `SetDefault(virtual)`,
    /// `PersistTarget`, `Status(On)`. Starting the new pipeline before the default switch means
    /// audio never goes to the virtual device without a consumer.
    fn restart_on(&mut self, id: DeviceId) -> Vec<Effect> {
        let Some(virtual_id) = self.virtual_id.clone() else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        if self.pipeline_active() {
            effects.push(Effect::StopPipeline);
        }
        self.set_target(Some(id.clone()));
        self.phase = Phase::On;
        effects.push(Effect::StartPipeline {
            virtual_id: virtual_id.clone(),
            target_id: id.clone(),
        });
        effects.push(Effect::SetDefault(virtual_id));
        effects.push(Effect::PersistTarget(id));
        effects.push(Effect::Status(RoutingStatus::On));
        effects
    }

    /// Whether a pipeline is (or was, if it failed) running and must be stopped.
    const fn pipeline_active(&self) -> bool {
        matches!(
            self.phase,
            Phase::On | Phase::Degraded(DegradedReason::PipelineFailed)
        )
    }

    /// Whether `id` is the virtual device, by id or by the device list flag.
    fn is_virtual(&self, id: &DeviceId) -> bool {
        self.virtual_id.as_ref() == Some(id)
            || self
                .devices
                .iter()
                .any(|device| device.is_virtual && device.id == *id)
    }

    /// Whether `id` is a known, non-virtual device.
    fn is_known_real(&self, id: &DeviceId) -> bool {
        !self.is_virtual(id) && self.devices.iter().any(|device| device.id == *id)
    }

    /// Chooses the target when power comes on: the default if it is a real device, otherwise
    /// the fallback order.
    fn initial_target(&self, virtual_id: &DeviceId) -> Option<DeviceId> {
        match &self.default_at_power_on {
            Some(default) if default != virtual_id && !self.is_virtual(default) => {
                Some(default.clone())
            }
            _ => self.fallback(),
        }
    }

    /// Chooses a fallback target: the current or a previous target that is still present
    /// (most recent first), otherwise the first real device in the list.
    fn fallback(&self) -> Option<DeviceId> {
        self.target
            .iter()
            .chain(self.history.iter())
            .find(|id| self.is_known_real(id))
            .cloned()
            .or_else(|| {
                self.devices
                    .iter()
                    .find(|device| !self.is_virtual(&device.id))
                    .map(|device| device.id.clone())
            })
    }

    /// Replaces the target and remembers the previous one in the history.
    fn set_target(&mut self, new: Option<DeviceId>) {
        if let Some(old) = self.target.take()
            && new.as_ref() != Some(&old)
        {
            self.history.retain(|id| *id != old);
            self.history.insert(0, old);
            self.history.truncate(HISTORY_LEN);
        }
        if let Some(id) = &new {
            self.history.retain(|old| old != id);
        }
        self.target = new;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> DeviceId {
        DeviceId::new(name)
    }

    fn real(name: &str) -> DeviceInfo {
        DeviceInfo {
            id: id(name),
            name: name.to_owned(),
            is_virtual: false,
        }
    }

    fn virtual_device() -> DeviceInfo {
        DeviceInfo {
            id: id("virtual"),
            name: "CABLE Input".to_owned(),
            is_virtual: true,
        }
    }

    /// The virtual device id used by every test.
    fn v() -> DeviceId {
        id("virtual")
    }

    fn devices_with(real_names: &[&str]) -> Vec<DeviceInfo> {
        let mut devices = vec![virtual_device()];
        devices.extend(real_names.iter().map(|name| real(name)));
        devices
    }

    /// Powers on with `default` and `devices`, then answers `EnsureVirtual` successfully.
    fn started(
        persisted: Option<DeviceId>,
        default: Option<DeviceId>,
        devices: Vec<DeviceInfo>,
    ) -> Router {
        let mut router = Router::new(persisted);
        assert_eq!(
            router.handle(RouterInput::PowerOn {
                current_default: default,
                devices,
            }),
            vec![Effect::EnsureVirtual]
        );
        let _ = router.handle(RouterInput::VirtualReady(v()));
        router
    }

    /// Routing is on, with `default` (a real device) as the target.
    fn started_on(real_names: &[&str], default: &str) -> Router {
        started(None, Some(id(default)), devices_with(real_names))
    }

    // Row: PowerOn -> EnsureVirtual.
    #[test]
    fn power_on_emits_ensure_virtual() {
        let mut router = Router::new(None);
        let effects = router.handle(RouterInput::PowerOn {
            current_default: Some(id("speakers")),
            devices: devices_with(&["speakers"]),
        });
        assert_eq!(effects, vec![Effect::EnsureVirtual]);
        assert_eq!(router.status(), RoutingStatus::Off);
    }

    // Row: PowerOn -> target = current default (not virtual) -> SetDefault -> StartPipeline.
    #[test]
    fn virtual_ready_routes_real_default_as_target() {
        let mut router = Router::new(None);
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(id("speakers")),
            devices: devices_with(&["speakers", "headphones"]),
        });
        assert_eq!(
            router.handle(RouterInput::VirtualReady(v())),
            vec![
                Effect::SetDefault(v()),
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("speakers"),
                },
                Effect::PersistTarget(id("speakers")),
                Effect::Status(RoutingStatus::On),
            ]
        );
        assert_eq!(router.status(), RoutingStatus::On);
        assert_eq!(router.target(), Some(&id("speakers")));
    }

    // Row: PowerOn while the default is already virtual -> persisted target.
    #[test]
    fn power_on_with_virtual_default_uses_persisted_target() {
        let mut router = Router::new(Some(id("headphones")));
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(v()),
            devices: devices_with(&["speakers", "headphones"]),
        });
        assert_eq!(
            router.handle(RouterInput::VirtualReady(v())),
            vec![
                Effect::SetDefault(v()),
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("headphones"),
                },
                Effect::PersistTarget(id("headphones")),
                Effect::Status(RoutingStatus::On),
            ]
        );
    }

    // Extra: default is virtual and no persisted target -> first real device in the list.
    #[test]
    fn power_on_with_virtual_default_and_no_persisted_target_uses_first_real_device() {
        let mut router = Router::new(None);
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(v()),
            devices: devices_with(&["speakers", "headphones"]),
        });
        assert_eq!(
            router.handle(RouterInput::VirtualReady(v())),
            vec![
                Effect::SetDefault(v()),
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("speakers"),
                },
                Effect::PersistTarget(id("speakers")),
                Effect::Status(RoutingStatus::On),
            ]
        );
    }

    // Extra: a persisted target that is no longer present is not used.
    #[test]
    fn persisted_target_missing_from_device_list_is_skipped() {
        let mut router = Router::new(Some(id("gone")));
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(v()),
            devices: devices_with(&["speakers"]),
        });
        assert_eq!(
            router.handle(RouterInput::VirtualReady(v())),
            vec![
                Effect::SetDefault(v()),
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("speakers"),
                },
                Effect::PersistTarget(id("speakers")),
                Effect::Status(RoutingStatus::On),
            ]
        );
    }

    // Extra: EnsureVirtual succeeds but there is no real device at all -> NoTarget.
    #[test]
    fn virtual_ready_without_any_target_degrades_no_target() {
        let mut router = Router::new(None);
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(v()),
            devices: devices_with(&[]),
        });
        assert_eq!(
            router.handle(RouterInput::VirtualReady(v())),
            vec![Effect::Status(RoutingStatus::Degraded(
                DegradedReason::NoTarget
            ))]
        );
        assert_eq!(
            router.status(),
            RoutingStatus::Degraded(DegradedReason::NoTarget)
        );
    }

    // Extra: EnsureVirtual failure -> Degraded(VirtualMissing).
    #[test]
    fn virtual_failed_degrades_virtual_missing() {
        let mut router = Router::new(None);
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(id("speakers")),
            devices: devices_with(&["speakers"]),
        });
        assert_eq!(
            router.handle(RouterInput::VirtualFailed),
            vec![Effect::Status(RoutingStatus::Degraded(
                DegradedReason::VirtualMissing
            ))]
        );
        assert_eq!(
            router.status(),
            RoutingStatus::Degraded(DegradedReason::VirtualMissing)
        );
    }

    // Stale answers and a repeated power-on are ignored.
    #[test]
    fn stale_virtual_answers_and_repeated_power_on_are_ignored() {
        let mut router = Router::new(None);
        assert_eq!(router.handle(RouterInput::VirtualReady(v())), vec![]);
        assert_eq!(router.handle(RouterInput::VirtualFailed), vec![]);

        let mut on = started_on(&["speakers"], "speakers");
        assert_eq!(
            on.handle(RouterInput::PowerOn {
                current_default: Some(id("speakers")),
                devices: devices_with(&["speakers"]),
            }),
            vec![]
        );
        assert_eq!(on.status(), RoutingStatus::On);
    }

    // Row: DefaultChanged(id), id != virtual, while On.
    #[test]
    fn default_changed_to_real_device_while_on_switches_target() {
        let mut router = started_on(&["speakers", "headphones"], "speakers");
        assert_eq!(
            router.handle(RouterInput::DefaultChanged(id("headphones"))),
            vec![
                Effect::StopPipeline,
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("headphones"),
                },
                Effect::SetDefault(v()),
                Effect::PersistTarget(id("headphones")),
                Effect::Status(RoutingStatus::On),
            ]
        );
        assert_eq!(router.target(), Some(&id("headphones")));
    }

    // Row: DefaultChanged(virtual) is our own change and is ignored.
    #[test]
    fn default_changed_to_virtual_is_ignored() {
        let mut router = started_on(&["speakers"], "speakers");
        assert_eq!(router.handle(RouterInput::DefaultChanged(v())), vec![]);
        assert_eq!(router.target(), Some(&id("speakers")));
    }

    // Extra: a repeated report of the current target while On does not restart the pipeline.
    #[test]
    fn default_changed_to_current_target_while_on_is_noop() {
        let mut router = started_on(&["speakers"], "speakers");
        assert_eq!(
            router.handle(RouterInput::DefaultChanged(id("speakers"))),
            vec![]
        );
    }

    // Row: DefaultChanged(x) while Off -> only remember x as the target candidate.
    #[test]
    fn default_changed_while_off_only_remembers_candidate() {
        let mut router = Router::new(None);
        // The controller announces the device list while off, so the virtual device is known.
        assert_eq!(
            router.handle(RouterInput::DeviceAdded(virtual_device())),
            vec![]
        );
        assert_eq!(
            router.handle(RouterInput::DefaultChanged(id("speakers"))),
            vec![]
        );
        assert_eq!(router.target(), Some(&id("speakers")));
        assert_eq!(router.status(), RoutingStatus::Off);
        assert_eq!(router.handle(RouterInput::DefaultChanged(v())), vec![]);
        assert_eq!(router.target(), Some(&id("speakers")));
    }

    // Row: DeviceRemoved(target) while On -> previous target first.
    #[test]
    fn device_removed_target_falls_back_to_previous_target() {
        let mut router = started_on(&["speakers", "headphones"], "speakers");
        let _ = router.handle(RouterInput::DefaultChanged(id("headphones")));
        assert_eq!(
            router.handle(RouterInput::DeviceRemoved(id("headphones"))),
            vec![
                Effect::StopPipeline,
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("speakers"),
                },
                Effect::SetDefault(v()),
                Effect::PersistTarget(id("speakers")),
                Effect::Status(RoutingStatus::On),
            ]
        );
        assert_eq!(router.target(), Some(&id("speakers")));
    }

    // Row: DeviceRemoved(target) while On, no history -> first real device in the list.
    #[test]
    fn device_removed_target_without_history_uses_first_real_device() {
        let mut router = started_on(&["speakers", "headphones", "usb"], "speakers");
        assert_eq!(
            router.handle(RouterInput::DeviceRemoved(id("speakers"))),
            vec![
                Effect::StopPipeline,
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("headphones"),
                },
                Effect::SetDefault(v()),
                Effect::PersistTarget(id("headphones")),
                Effect::Status(RoutingStatus::On),
            ]
        );
    }

    // Row: DeviceRemoved(target) with no other device -> Degraded(NoTarget).
    #[test]
    fn device_removed_target_with_no_other_device_degrades_no_target() {
        let mut router = started_on(&["speakers"], "speakers");
        assert_eq!(
            router.handle(RouterInput::DeviceRemoved(id("speakers"))),
            vec![
                Effect::StopPipeline,
                Effect::Status(RoutingStatus::Degraded(DegradedReason::NoTarget)),
            ]
        );
        assert_eq!(
            router.status(),
            RoutingStatus::Degraded(DegradedReason::NoTarget)
        );
        assert_eq!(router.target(), None);
    }

    // Row: DeviceRemoved(virtual) -> StopPipeline, Degraded(VirtualMissing).
    #[test]
    fn device_removed_virtual_degrades_virtual_missing() {
        let mut router = started_on(&["speakers"], "speakers");
        assert_eq!(
            router.handle(RouterInput::DeviceRemoved(v())),
            vec![
                Effect::StopPipeline,
                Effect::Status(RoutingStatus::Degraded(DegradedReason::VirtualMissing)),
            ]
        );
    }

    // Extra: after the virtual device disappeared, power off must not stop a pipeline that
    // is no longer running, but still restores the default.
    #[test]
    fn power_off_after_virtual_removed_restores_default_without_stop() {
        let mut router = started_on(&["speakers"], "speakers");
        let _ = router.handle(RouterInput::DeviceRemoved(v()));
        assert_eq!(
            router.handle(RouterInput::PowerOff),
            vec![
                Effect::SetDefault(id("speakers")),
                Effect::ReleaseVirtual,
                Effect::Status(RoutingStatus::Off),
            ]
        );
        assert_eq!(router.status(), RoutingStatus::Off);
    }

    // Row: removal of a device that is neither target nor virtual has no effect.
    #[test]
    fn device_removed_non_target_is_ignored() {
        let mut router = started_on(&["speakers", "headphones"], "speakers");
        assert_eq!(
            router.handle(RouterInput::DeviceRemoved(id("headphones"))),
            vec![]
        );
        assert_eq!(router.target(), Some(&id("speakers")));
        assert_eq!(router.status(), RoutingStatus::On);
    }

    // Row: PowerOff -> StopPipeline -> SetDefault(target) -> ReleaseVirtual.
    #[test]
    fn power_off_while_on_restores_target_and_releases_virtual() {
        let mut router = started_on(&["speakers"], "speakers");
        assert_eq!(
            router.handle(RouterInput::PowerOff),
            vec![
                Effect::StopPipeline,
                Effect::SetDefault(id("speakers")),
                Effect::ReleaseVirtual,
                Effect::Status(RoutingStatus::Off),
            ]
        );
        assert_eq!(router.status(), RoutingStatus::Off);
    }

    // Extra: power off while already off does nothing.
    #[test]
    fn power_off_while_off_is_noop() {
        let mut router = Router::new(None);
        assert_eq!(router.handle(RouterInput::PowerOff), vec![]);
    }

    // Extra: a failed pipeline is reported, and a successful retry brings routing back on.
    #[test]
    fn pipeline_failed_then_recovered() {
        let mut router = started_on(&["speakers"], "speakers");
        assert_eq!(
            router.handle(RouterInput::PipelineFailed),
            vec![Effect::Status(RoutingStatus::Degraded(
                DegradedReason::PipelineFailed
            ))]
        );
        assert_eq!(
            router.handle(RouterInput::PipelineRecovered),
            vec![Effect::Status(RoutingStatus::On)]
        );
    }

    // Extra: a pipeline failure while off is stale and ignored.
    #[test]
    fn pipeline_failed_while_off_is_ignored() {
        let mut router = Router::new(None);
        assert_eq!(router.handle(RouterInput::PipelineFailed), vec![]);
        assert_eq!(router.handle(RouterInput::PipelineRecovered), vec![]);
    }

    // Extra: after a failure the old pipeline handle is still stopped before the new one starts,
    // so the controller releases its resources.
    #[test]
    fn default_changed_after_pipeline_failure_stops_then_restarts() {
        let mut router = started_on(&["speakers", "headphones"], "speakers");
        let _ = router.handle(RouterInput::PipelineFailed);
        assert_eq!(
            router.handle(RouterInput::DefaultChanged(id("headphones"))),
            vec![
                Effect::StopPipeline,
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("headphones"),
                },
                Effect::SetDefault(v()),
                Effect::PersistTarget(id("headphones")),
                Effect::Status(RoutingStatus::On),
            ]
        );
    }

    // Extra: a device added while degraded for lack of a target resumes routing.
    #[test]
    fn device_added_resolves_no_target_degradation() {
        let mut router = started(None, None, devices_with(&[]));
        assert_eq!(
            router.status(),
            RoutingStatus::Degraded(DegradedReason::NoTarget)
        );
        assert_eq!(
            router.handle(RouterInput::DeviceAdded(real("headphones"))),
            vec![
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("headphones"),
                },
                Effect::SetDefault(v()),
                Effect::PersistTarget(id("headphones")),
                Effect::Status(RoutingStatus::On),
            ]
        );
    }

    // Extra: a device added while routing works is only listed.
    #[test]
    fn device_added_while_on_has_no_effect() {
        let mut router = started_on(&["speakers"], "speakers");
        assert_eq!(
            router.handle(RouterInput::DeviceAdded(real("headphones"))),
            vec![]
        );
        assert_eq!(router.target(), Some(&id("speakers")));
    }

    // Extra: the history keeps only the last four previous targets.
    #[test]
    fn history_keeps_last_four_targets() {
        let names = ["a", "b", "c", "d", "e", "f"];
        let mut router = started_on(&names, "a");
        for name in &names[1..] {
            let _ = router.handle(RouterInput::DefaultChanged(id(name)));
        }
        assert_eq!(router.target(), Some(&id("f")));
        assert_eq!(router.history, vec![id("e"), id("d"), id("c"), id("b")]);
    }

    // Extra: after the target is removed, the most recent previous target that still exists
    // wins over the first device in the list.
    #[test]
    fn previous_target_is_preferred_over_list_order() {
        let mut router = started_on(&["first", "a", "b"], "a");
        let _ = router.handle(RouterInput::DefaultChanged(id("b")));
        assert_eq!(
            router.handle(RouterInput::DeviceRemoved(id("b"))),
            vec![
                Effect::StopPipeline,
                Effect::StartPipeline {
                    virtual_id: v(),
                    target_id: id("a"),
                },
                Effect::SetDefault(v()),
                Effect::PersistTarget(id("a")),
                Effect::Status(RoutingStatus::On),
            ]
        );
    }

    // Extra: power off from Degraded(NoTarget) only releases the virtual device.
    #[test]
    fn power_off_from_no_target_only_releases_virtual() {
        let mut router = started(None, None, devices_with(&[]));
        assert_eq!(
            router.handle(RouterInput::PowerOff),
            vec![Effect::ReleaseVirtual, Effect::Status(RoutingStatus::Off)]
        );
    }

    // Extra: power off from Degraded(VirtualMissing) with a known target restores the default.
    #[test]
    fn power_off_from_virtual_missing_restores_known_target() {
        let mut router = Router::new(None);
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(id("speakers")),
            devices: devices_with(&["speakers"]),
        });
        let _ = router.handle(RouterInput::VirtualFailed);
        assert_eq!(
            router.handle(RouterInput::PowerOff),
            vec![
                Effect::SetDefault(id("speakers")),
                Effect::ReleaseVirtual,
                Effect::Status(RoutingStatus::Off),
            ]
        );
    }

    // Extra: while Ensuring, the default may change; the new real default becomes the target.
    #[test]
    fn default_changed_during_ensure_is_used_for_target() {
        let mut router = Router::new(None);
        let _ = router.handle(RouterInput::PowerOn {
            current_default: Some(v()),
            devices: devices_with(&["speakers", "headphones"]),
        });
        assert_eq!(
            router.handle(RouterInput::DefaultChanged(id("headphones"))),
            vec![]
        );
        assert!(
            router
                .handle(RouterInput::VirtualReady(v()))
                .contains(&Effect::PersistTarget(id("headphones")))
        );
    }
}
