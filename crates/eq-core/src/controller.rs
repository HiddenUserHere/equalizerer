//! The controller: the one thread that owns the router, the audio backend, the presets and the
//! settings, and connects them to the UI.
//!
//! The controller is an actor. It receives [`AppCommand`]s from the UI and [`DeviceEvent`]s from
//! the backend, feeds the [`Router`] with [`RouterInput`]s, executes the returned [`Effect`]s
//! through the ports, and answers the UI with [`AppEvent`]s. Only this thread touches the
//! backend, so the adapters need no locking and need not be `Send`.

use std::collections::VecDeque;
use std::io;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, never, select};
use eq_dsp::{EqParams, Layout, MAX_BANDS};

use crate::messages::{AppCommand, AppEvent, AppSnapshot, DeviceEvent};
use crate::model::{DeviceId, DeviceInfo, LatencyMode, LayoutId, RoutingStatus};
use crate::ports::{AudioBackend, BackendError, Pipeline, PipelineConfig, PipelineError};
use crate::presets::{Preset, PresetStore, validate_gains, validate_name};
use crate::router::{Effect, Router, RouterInput};
use crate::settings::{Settings, SettingsStore};

/// How long the loop waits when no deadline is pending. Any command or event wakes it earlier.
const IDLE_WAIT: Duration = Duration::from_secs(3600);

/// The only layout the app supports today; presets and the EQ use the same one.
const LAYOUT_ID: LayoutId = LayoutId::Ten;

/// Number of band gains the controller keeps.
const BAND_COUNT: usize = Layout::Ten.len();

/// Timing and retry parameters of the controller.
///
/// The defaults are the values from `docs/PLAN.md` T-3.7; tests use much smaller ones.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::module_name_repetitions,
    reason = "the type name `ControllerConfig` is fixed by docs/PLAN.md T-3.7"
)]
pub struct ControllerConfig {
    /// Quiet time before a `DefaultChanged` event is acted on (Windows sends one per role).
    pub debounce: Duration,
    /// Interval between `AppEvent::Stats` while a pipeline runs.
    pub stats_interval: Duration,
    /// Quiet time before settings are written after a change.
    pub save_delay: Duration,
    /// Delays before each retry after a pipeline failure. Exhausting the list degrades routing.
    pub retry_backoff: Vec<Duration>,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            debounce: Duration::from_millis(150),
            stats_interval: Duration::from_millis(250),
            save_delay: Duration::from_millis(500),
            retry_backoff: vec![
                Duration::from_millis(100),
                Duration::from_millis(500),
                Duration::from_secs(2),
            ],
        }
    }
}

/// What woke the event loop.
enum Wakeup {
    /// The UI sent a command.
    Command(AppCommand),
    /// The UI side dropped its sender.
    CommandsClosed,
    /// The backend reported something.
    Device(DeviceEvent),
    /// The backend side dropped its sender.
    DevicesClosed,
    /// A deadline is due (or the idle wait ended).
    Timeout,
}

/// Owns the routing state and executes its effects. See the module documentation.
pub struct Controller {
    backend: Box<dyn AudioBackend>,
    presets: PresetStore,
    settings_store: SettingsStore,
    settings: Settings,
    config: ControllerConfig,
    events_out: Sender<AppEvent>,
    router: Router,
    /// Router inputs waiting to be handled. Effects may enqueue follow-up inputs.
    inputs: VecDeque<RouterInput>,
    /// Last device list read from the backend.
    devices: Vec<DeviceInfo>,
    /// Routing status as last reported by the router.
    status: RoutingStatus,
    /// Current band gains in dB.
    gains: Vec<f32>,
    /// Preset the gains were loaded from, if it still exists.
    active_preset: Option<String>,
    /// `true` when the gains were changed after loading the active preset.
    modified: bool,
    /// The running pipeline, if any.
    pipeline: Option<Box<dyn Pipeline>>,
    /// Virtual device of the last started pipeline; retries reuse it.
    pipeline_virtual: Option<DeviceId>,
    /// Latest default-device change and the time it may be acted on.
    pending_default: Option<(DeviceId, Instant)>,
    /// Time at which settings are written.
    save_due: Option<Instant>,
    /// Time of the next `Stats` event while a pipeline runs.
    stats_due: Option<Instant>,
    /// Time of the next pipeline retry.
    retry_due: Option<Instant>,
    /// Index into `retry_backoff` of the next retry; reset on success.
    retry_attempt: usize,
}

impl Controller {
    /// Creates a controller around an already built backend.
    ///
    /// Gains and the active preset are taken from `settings`; an active preset that no longer
    /// exists is dropped. Nothing is started until [`Controller::run`].
    #[must_use]
    pub fn new(
        backend: Box<dyn AudioBackend>,
        presets: PresetStore,
        settings_store: SettingsStore,
        settings: Settings,
        config: ControllerConfig,
        events_out: Sender<AppEvent>,
    ) -> Self {
        let gains = validate_gains(LAYOUT_ID, settings.current_gains.clone())
            .unwrap_or_else(|_| vec![0.0; BAND_COUNT]);
        let active_preset = settings
            .active_preset
            .clone()
            .filter(|name| presets.get(name).is_some());
        let modified = active_preset
            .as_deref()
            .and_then(|name| presets.get(name))
            .is_some_and(|preset| preset.gains != gains);
        let router = Router::new(settings.target_device_id.clone());
        Self {
            backend,
            presets,
            settings_store,
            settings,
            config,
            events_out,
            router,
            inputs: VecDeque::new(),
            devices: Vec::new(),
            status: RoutingStatus::Off,
            gains,
            active_preset,
            modified,
            pipeline: None,
            pipeline_virtual: None,
            pending_default: None,
            save_due: None,
            stats_due: None,
            retry_due: None,
            retry_attempt: 0,
        }
    }

    /// Starts the controller on a new thread named `controller`.
    ///
    /// The backend is built by `make_backend` on that thread, because OS objects (COM
    /// apartments, the `PipeWire` loop) must be created and used on one thread. If the factory
    /// fails, an [`AppEvent::Error`] is sent and the thread ends.
    ///
    /// # Errors
    ///
    /// Returns the I/O error when the thread cannot be created.
    #[allow(
        clippy::too_many_arguments,
        reason = "the signature is fixed by the T-3.7 design; every argument is a distinct owned input moved into the thread"
    )]
    pub fn spawn<F>(
        make_backend: F,
        presets: PresetStore,
        settings_store: SettingsStore,
        settings: Settings,
        config: ControllerConfig,
        events_out: Sender<AppEvent>,
        commands: Receiver<AppCommand>,
        device_events: Receiver<DeviceEvent>,
    ) -> io::Result<JoinHandle<()>>
    where
        F: FnOnce() -> Result<Box<dyn AudioBackend>, BackendError> + Send + 'static,
    {
        thread::Builder::new()
            .name("controller".to_owned())
            .spawn(move || match make_backend() {
                Ok(backend) => Self::new(
                    backend,
                    presets,
                    settings_store,
                    settings,
                    config,
                    events_out,
                )
                .run(commands, device_events),
                Err(err) => {
                    // The UI is told why; the thread ends, which the UI sees as a closed event
                    // channel.
                    let _ = events_out.send(AppEvent::Error(err.to_string()));
                }
            })
    }

    /// Runs the event loop until [`AppCommand::Quit`] arrives or both channels close.
    ///
    /// Publishes one snapshot first, restores power if the settings say it was on, and on exit
    /// powers off (the real device becomes the default again) while keeping the persisted power
    /// state "on". Settings are saved before returning.
    pub fn run(mut self, commands: Receiver<AppCommand>, device_events: Receiver<DeviceEvent>) {
        self.load_devices();
        self.publish();
        if self.settings.power_on {
            self.power_on();
            self.settle();
        }
        let mut commands = Some(commands);
        let mut device_events = Some(device_events);
        while commands.is_some() || device_events.is_some() {
            let timeout = self.time_to_next_deadline(Instant::now());
            let wakeup = wait_for_input(commands.as_ref(), device_events.as_ref(), timeout);
            match wakeup {
                Wakeup::Command(AppCommand::Quit) => break,
                Wakeup::Command(command) => self.handle_command(command),
                Wakeup::Device(event) => self.handle_device_event(event),
                Wakeup::CommandsClosed => commands = None,
                Wakeup::DevicesClosed => device_events = None,
                Wakeup::Timeout => {}
            }
            if self.run_due(Instant::now()) {
                self.settle();
            }
        }
        self.shutdown();
    }

    /// Powers off for shutdown and writes the settings. The persisted power state is kept.
    fn shutdown(&mut self) {
        self.power_off();
        self.save_settings();
    }

    /// Applies one UI command and publishes the new state.
    fn handle_command(&mut self, command: AppCommand) {
        match command {
            AppCommand::SetPower(on) => self.set_power(on),
            AppCommand::SetGain { band, db } => self.set_gain(band, db),
            AppCommand::SetGains(gains) => self.set_gains(gains),
            AppCommand::SelectPreset(name) => self.select_preset(&name),
            AppCommand::SavePreset => self.save_preset(),
            AppCommand::SavePresetAs(name) => self.save_preset_as(&name),
            AppCommand::RenamePreset { from, to } => self.rename_preset(&from, &to),
            AppCommand::DeletePreset(name) => self.delete_preset(&name),
            AppCommand::SetLatency(mode) => self.set_latency(mode),
            AppCommand::SelectTarget(id) => self.dispatch(RouterInput::UserSelectedTarget(id)),
            // The window belongs to the UI, and `run` handles Quit before it reaches this
            // function, so neither needs work here.
            AppCommand::ShowWindow | AppCommand::Quit => {}
        }
        self.settle();
    }

    /// Applies one backend event and publishes the new state.
    fn handle_device_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::DefaultChanged(id) => {
                // Each new report replaces the pending one and moves the deadline, so a burst
                // of reports produces one restart.
                self.pending_default = Some((id, Instant::now() + self.config.debounce));
            }
            DeviceEvent::Added(id) => self.device_added(&id),
            DeviceEvent::Removed(id) => {
                self.refresh_devices();
                self.dispatch(RouterInput::DeviceRemoved(id));
            }
            DeviceEvent::PipelineFailed(message) => self.pipeline_failed(message),
        }
        self.settle();
    }

    /// Runs the deadlines that are due. Returns `true` when routing or presets may have changed.
    fn run_due(&mut self, now: Instant) -> bool {
        let mut changed = false;
        if let Some((id, due)) = self.pending_default.take() {
            if due <= now {
                self.dispatch(RouterInput::DefaultChanged(id));
                changed = true;
            } else {
                self.pending_default = Some((id, due));
            }
        }
        if self.save_due.is_some_and(|due| due <= now) {
            self.save_settings();
        }
        if self.stats_due.is_some_and(|due| due <= now) {
            self.emit_stats(now);
        }
        if self.retry_due.is_some_and(|due| due <= now) {
            self.attempt_retry();
            changed = true;
        }
        changed
    }

    /// The nearest pending deadline, or the idle wait when nothing is pending.
    fn time_to_next_deadline(&self, now: Instant) -> Duration {
        let next = [
            self.pending_default.as_ref().map(|(_, due)| *due),
            self.save_due,
            self.stats_due,
            self.retry_due,
        ]
        .into_iter()
        .flatten()
        .min();
        next.map_or(IDLE_WAIT, |due| due.saturating_duration_since(now))
    }

    /// Sends a `Stats` event if a pipeline runs, and schedules the next one.
    fn emit_stats(&mut self, now: Instant) {
        let Some(pipeline) = self.pipeline.as_ref() else {
            self.stats_due = None;
            return;
        };
        let stats = pipeline.stats();
        self.stats_due = Some(now + self.config.stats_interval);
        self.send(AppEvent::Stats(stats));
    }

    /// Feeds one input to the router and executes what it returns.
    fn dispatch(&mut self, input: RouterInput) {
        self.inputs.push_back(input);
        self.drain();
    }

    /// Handles queued router inputs until none are left. Effects may add more.
    fn drain(&mut self) {
        while let Some(input) = self.inputs.pop_front() {
            for effect in self.router.handle(input) {
                self.execute(effect);
            }
        }
    }

    /// Finishes a step: handles everything queued and publishes the snapshot.
    fn settle(&mut self) {
        self.drain();
        self.publish();
    }

    /// Executes one router effect. Failures are reported and turned into router inputs.
    fn execute(&mut self, effect: Effect) {
        match effect {
            Effect::EnsureVirtual => self.ensure_virtual(),
            Effect::ReleaseVirtual => self.release_virtual(),
            Effect::SetDefault(id) => self.set_default(&id),
            Effect::StartPipeline {
                virtual_id,
                target_id,
            } => self.start_or_retry(virtual_id, target_id),
            Effect::StopPipeline => {
                self.stop_pipeline();
                self.cancel_retry();
            }
            Effect::PersistTarget(id) => {
                self.settings.target_device_id = Some(id);
                self.mark_dirty();
            }
            Effect::Status(status) => self.status = status,
        }
    }

    /// Makes the virtual device available and reports the answer to the router.
    fn ensure_virtual(&mut self) {
        let result = self.backend.virtual_provider().ensure(&self.devices);
        match result {
            Ok(id) => self.inputs.push_back(RouterInput::VirtualReady(id)),
            Err(err) => {
                self.report(err.to_string());
                self.inputs.push_back(RouterInput::VirtualFailed);
            }
        }
    }

    /// Releases the virtual device. Errors are reported; routing has already moved on.
    fn release_virtual(&mut self) {
        let result = self.backend.virtual_provider().release();
        if let Err(err) = result {
            self.report(err.to_string());
        }
    }

    /// Changes the system default output.
    fn set_default(&mut self, id: &DeviceId) {
        let result = self.backend.directory().set_default_render(id);
        if let Err(err) = result {
            self.report(err.to_string());
        }
    }

    /// Starts a pipeline for a router effect. A failure schedules the first retry.
    fn start_or_retry(&mut self, virtual_id: DeviceId, target_id: DeviceId) {
        self.cancel_retry();
        if let Err(err) = self.start_pipeline(virtual_id, target_id) {
            self.report(err.to_string());
            self.retry_attempt = 0;
            self.schedule_retry();
        }
    }

    /// Builds and starts a pipeline with the current latency mode and EQ.
    ///
    /// Any pipeline still held is stopped first, so at most one runs at a time.
    fn start_pipeline(
        &mut self,
        virtual_id: DeviceId,
        target_id: DeviceId,
    ) -> Result<(), PipelineError> {
        self.stop_pipeline();
        // Recorded before the start, so a failed start can still be retried with this device.
        self.pipeline_virtual = Some(virtual_id.clone());
        let config = PipelineConfig {
            virtual_id,
            target_id,
            latency: self.settings.latency_mode,
            eq: self.eq_params(),
        };
        let pipeline = self.backend.pipeline_factory().start(config)?;
        self.pipeline = Some(pipeline);
        self.stats_due = Some(Instant::now() + self.config.stats_interval);
        Ok(())
    }

    /// Stops and drops the running pipeline, if any.
    fn stop_pipeline(&mut self) {
        self.stats_due = None;
        if let Some(pipeline) = self.pipeline.take() {
            pipeline.stop();
        }
    }

    /// A running pipeline reported a failure. It is stopped and retried with the backoff.
    fn pipeline_failed(&mut self, message: String) {
        // A failure while no pipeline runs is stale (for example, it raced with power off).
        if self.pipeline.is_none() {
            return;
        }
        self.report(message);
        self.stop_pipeline();
        self.retry_attempt = 0;
        self.schedule_retry();
    }

    /// Schedules the next retry after the backoff for `retry_attempt`. When the backoff is
    /// exhausted the router is told, which degrades routing.
    fn schedule_retry(&mut self) {
        if let Some(delay) = self.config.retry_backoff.get(self.retry_attempt).copied() {
            self.retry_due = Some(Instant::now() + delay);
        } else {
            self.cancel_retry();
            self.inputs.push_back(RouterInput::PipelineFailed);
        }
    }

    /// Forgets any pending retry.
    fn cancel_retry(&mut self) {
        self.retry_due = None;
        self.retry_attempt = 0;
    }

    /// Restarts the pipeline with the router's current target and the last virtual device.
    fn attempt_retry(&mut self) {
        self.retry_due = None;
        let (Some(virtual_id), Some(target_id)) =
            (self.pipeline_virtual.clone(), self.router.target().cloned())
        else {
            self.cancel_retry();
            self.inputs.push_back(RouterInput::PipelineFailed);
            return;
        };
        match self.start_pipeline(virtual_id, target_id) {
            Ok(()) => {
                self.cancel_retry();
                self.inputs.push_back(RouterInput::PipelineRecovered);
            }
            Err(err) => {
                self.report(err.to_string());
                self.retry_attempt += 1;
                self.schedule_retry();
            }
        }
    }

    /// Turns power on: reads the OS state and lets the router choose the target.
    fn power_on(&mut self) {
        self.refresh_devices();
        let default_result = self.backend.directory().default_render();
        let current_default = match default_result {
            Ok(id) => id,
            Err(err) => {
                self.report(err.to_string());
                None
            }
        };
        let devices = self.devices.clone();
        self.dispatch(RouterInput::PowerOn {
            current_default,
            devices,
        });
    }

    /// Turns power off: the router stops the pipeline, restores the target and releases the
    /// virtual device. The stop and cancel calls cover a retry that the router does not know of.
    fn power_off(&mut self) {
        self.dispatch(RouterInput::PowerOff);
        self.stop_pipeline();
        self.cancel_retry();
    }

    /// Handles the power button and persists the choice.
    fn set_power(&mut self, on: bool) {
        self.settings.power_on = on;
        self.mark_dirty();
        if on {
            self.power_on();
        } else {
            self.power_off();
        }
    }

    /// Handles a device that appeared. Only devices present in the refreshed list are passed on.
    fn device_added(&mut self, id: &DeviceId) {
        self.refresh_devices();
        let info = self.devices.iter().find(|device| device.id == *id).cloned();
        if let Some(info) = info {
            self.dispatch(RouterInput::DeviceAdded(info));
        }
    }

    /// Re-reads the device list from the backend.
    fn refresh_devices(&mut self) {
        let result = self.backend.directory().list_render();
        match result {
            Ok(devices) => self.devices = devices,
            Err(err) => self.report(err.to_string()),
        }
    }

    /// Reads the device list at start and tells the router about each device. The router needs
    /// the outputs before power-on to accept a choice made in the UI while power is off. While
    /// off, `DeviceAdded` has no effect, so this causes no routing change.
    fn load_devices(&mut self) {
        self.refresh_devices();
        for info in self.devices.clone() {
            self.dispatch(RouterInput::DeviceAdded(info));
        }
    }

    /// Sets one band's gain.
    fn set_gain(&mut self, band: usize, db: f32) {
        let mut gains = self.gains.clone();
        let Some(gain) = gains.get_mut(band) else {
            self.report(format!("band {band} does not exist"));
            return;
        };
        *gain = db;
        self.set_gains(gains);
    }

    /// Replaces all gains. Editing marks the active preset as modified.
    fn set_gains(&mut self, gains: Vec<f32>) {
        match validate_gains(LAYOUT_ID, gains) {
            Ok(gains) => {
                let modified = self.active_preset.is_some();
                self.apply_gains(gains, modified);
            }
            Err(err) => self.report(err.to_string()),
        }
    }

    /// Stores the gains, persists them, and pushes them to the running pipeline.
    fn apply_gains(&mut self, gains: Vec<f32>, modified: bool) {
        self.gains = gains;
        self.modified = modified;
        self.settings.current_gains = self.gains.clone();
        self.mark_dirty();
        self.push_eq();
    }

    /// Sends the current EQ parameters to the running pipeline, if there is one.
    fn push_eq(&mut self) {
        let eq = self.eq_params();
        if let Some(pipeline) = self.pipeline.as_mut() {
            pipeline.set_eq(eq);
        }
    }

    /// The EQ parameters for the current gains and power state.
    fn eq_params(&self) -> EqParams {
        let mut params = EqParams {
            layout: Layout::Ten,
            gains: [0.0; MAX_BANDS],
            enabled: self.settings.power_on,
        };
        for (dst, src) in params.gains.iter_mut().zip(&self.gains) {
            *dst = *src;
        }
        params
    }

    /// Loads a preset's gains and makes it the active one.
    fn select_preset(&mut self, name: &str) {
        match self.presets.get(name) {
            Some(preset) => {
                self.set_active(Some(preset.name.clone()));
                self.apply_gains(preset.gains, false);
            }
            None => self.report(format!("no preset named `{name}`")),
        }
    }

    /// Overwrites the active user preset with the current gains.
    fn save_preset(&mut self) {
        let Some(name) = self.active_preset.clone() else {
            self.report("no preset is selected; use Save as".to_owned());
            return;
        };
        if self.presets.get(&name).is_some_and(|preset| preset.builtin) {
            self.report(format!("`{name}` is built in; use Save as"));
            return;
        }
        if self.store_preset(name) {
            self.modified = false;
        }
    }

    /// Creates a user preset from the current gains and makes it active.
    fn save_preset_as(&mut self, name: &str) {
        let name = match validate_name(name) {
            Ok(name) => name,
            Err(err) => {
                self.report(err.to_string());
                return;
            }
        };
        if self.presets.get(&name).is_some() {
            self.report(format!("a preset named `{name}` already exists"));
            return;
        }
        if self.store_preset(name.clone()) {
            self.set_active(Some(name));
            self.modified = false;
        }
    }

    /// Writes the current gains as the preset `name`. Returns `false` after reporting an error.
    fn store_preset(&mut self, name: String) -> bool {
        let preset = Preset {
            name,
            layout: LAYOUT_ID,
            gains: self.gains.clone(),
            builtin: false,
        };
        match self.presets.save(preset) {
            Ok(()) => true,
            Err(err) => {
                self.report(err.to_string());
                false
            }
        }
    }

    /// Renames a user preset and keeps the active name in step.
    fn rename_preset(&mut self, from: &str, to: &str) {
        match self.presets.rename(from, to) {
            Ok(()) => {
                if self.is_active(from) {
                    self.set_active(Some(to.trim().to_owned()));
                }
            }
            Err(err) => self.report(err.to_string()),
        }
    }

    /// Deletes a user preset. Deleting the active one leaves no preset active.
    fn delete_preset(&mut self, name: &str) {
        match self.presets.delete(name) {
            Ok(()) => {
                if self.is_active(name) {
                    self.set_active(None);
                    self.modified = false;
                }
            }
            Err(err) => self.report(err.to_string()),
        }
    }

    /// Whether `name` is the active preset (names are compared case-insensitively).
    fn is_active(&self, name: &str) -> bool {
        self.active_preset
            .as_ref()
            .is_some_and(|active| active.to_lowercase() == name.trim().to_lowercase())
    }

    /// Sets the active preset in memory and in the settings.
    fn set_active(&mut self, name: Option<String>) {
        self.settings.active_preset.clone_from(&name);
        self.active_preset = name;
        self.mark_dirty();
    }

    /// Changes the latency mode. A running pipeline is restarted with the new mode.
    fn set_latency(&mut self, mode: LatencyMode) {
        self.settings.latency_mode = mode;
        self.mark_dirty();
        if self.pipeline.is_some() {
            self.restart_pipeline();
        }
    }

    /// Stops the running pipeline and starts it again with the router's target.
    fn restart_pipeline(&mut self) {
        self.stop_pipeline();
        let (Some(virtual_id), Some(target_id)) =
            (self.pipeline_virtual.clone(), self.router.target().cloned())
        else {
            return;
        };
        self.start_or_retry(virtual_id, target_id);
    }

    /// Requests a settings save after the save delay. Repeated changes postpone it.
    fn mark_dirty(&mut self) {
        self.save_due = Some(Instant::now() + self.config.save_delay);
    }

    /// Writes the settings now. A failure is reported; the next change tries again.
    fn save_settings(&mut self) {
        self.save_due = None;
        if let Err(err) = self.settings_store.save(&self.settings) {
            self.report(err.to_string());
        }
    }

    /// Builds the snapshot the UI renders.
    fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            power: self.settings.power_on,
            status: self.status,
            target_name: self.target_name(),
            target_id: self.router.target().cloned(),
            devices: self
                .devices
                .iter()
                .filter(|device| !device.is_virtual)
                .cloned()
                .collect(),
            presets: self.presets.list(),
            active_preset: self.active_preset.clone(),
            modified: self.modified,
            gains: self.gains.clone(),
            latency: self.settings.latency_mode,
        }
    }

    /// Name of the router's target, looked up in the last device list.
    fn target_name(&self) -> Option<String> {
        let target = self.router.target()?;
        self.devices
            .iter()
            .find(|device| device.id == *target)
            .map(|device| device.name.clone())
    }

    /// Sends the current snapshot to the UI.
    fn publish(&self) {
        self.send(AppEvent::Snapshot(self.snapshot()));
    }

    /// Sends an error message to the UI.
    fn report(&self, message: String) {
        self.send(AppEvent::Error(message));
    }

    /// Sends an event to the UI. A closed UI channel is ignored; the loop ends when the UI's
    /// command channel closes.
    fn send(&self, event: AppEvent) {
        let _ = self.events_out.send(event);
    }
}

/// Waits for the next command, device event or timeout.
///
/// A disconnected channel is always ready in `select!`, so a closed channel is replaced by one
/// that never fires, and the loop stops selecting it once it is closed.
fn wait_for_input(
    commands: Option<&Receiver<AppCommand>>,
    device_events: Option<&Receiver<DeviceEvent>>,
    timeout: Duration,
) -> Wakeup {
    let idle_commands: Receiver<AppCommand> = never();
    let idle_events: Receiver<DeviceEvent> = never();
    let commands = commands.unwrap_or(&idle_commands);
    let device_events = device_events.unwrap_or(&idle_events);
    select! {
        recv(commands) -> message => message.map_or(Wakeup::CommandsClosed, Wakeup::Command),
        recv(device_events) -> message => message.map_or(Wakeup::DevicesClosed, Wakeup::Device),
        default(timeout) => Wakeup::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex, MutexGuard};

    use crossbeam_channel::unbounded;

    use super::*;
    use crate::model::{DegradedReason, Stats};
    use crate::ports::{EndpointDirectory, PipelineFactory, VirtualDeviceProvider, VirtualError};

    /// Upper bound for waiting on an event; every expected event arrives long before this.
    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Gains of the built-in "Rock" preset.
    const ROCK: [f32; 10] = [4.0, 3.0, 1.0, -1.0, -2.0, -1.0, 1.0, 3.0, 4.0, 4.0];

    /// State shared by all fakes. The controller thread writes it; the test reads it at the end.
    #[derive(Default)]
    struct Shared {
        /// Ordered log of port calls, for example `set_default:speakers`.
        calls: Vec<String>,
        /// Every EQ parameter block pushed to a pipeline.
        eq_pushes: Vec<EqParams>,
        default: Option<DeviceId>,
        devices: Vec<DeviceInfo>,
        /// How many of the next pipeline starts fail.
        start_failures_left: usize,
    }

    type State = Arc<Mutex<Shared>>;

    fn lock(state: &State) -> MutexGuard<'_, Shared> {
        state.lock().expect("fake state lock")
    }

    /// What a test reads back after the controller has stopped.
    struct Outcome {
        calls: Vec<String>,
        eq_pushes: Vec<EqParams>,
    }

    struct FakeBackend {
        directory: FakeDirectory,
        provider: FakeProvider,
        factory: FakeFactory,
    }

    impl FakeBackend {
        fn new(state: State) -> Self {
            Self {
                directory: FakeDirectory {
                    state: Arc::clone(&state),
                },
                provider: FakeProvider {
                    state: Arc::clone(&state),
                },
                factory: FakeFactory { state },
            }
        }
    }

    impl AudioBackend for FakeBackend {
        fn directory(&self) -> &dyn EndpointDirectory {
            &self.directory
        }

        fn virtual_provider(&mut self) -> &mut dyn VirtualDeviceProvider {
            &mut self.provider
        }

        fn pipeline_factory(&self) -> &dyn PipelineFactory {
            &self.factory
        }
    }

    struct FakeDirectory {
        state: State,
    }

    impl EndpointDirectory for FakeDirectory {
        fn list_render(&self) -> Result<Vec<DeviceInfo>, BackendError> {
            Ok(lock(&self.state).devices.clone())
        }

        fn default_render(&self) -> Result<Option<DeviceId>, BackendError> {
            Ok(lock(&self.state).default.clone())
        }

        fn set_default_render(&self, id: &DeviceId) -> Result<(), BackendError> {
            let mut shared = lock(&self.state);
            shared.calls.push(format!("set_default:{id}"));
            shared.default = Some(id.clone());
            Ok(())
        }
    }

    struct FakeProvider {
        state: State,
    }

    impl VirtualDeviceProvider for FakeProvider {
        fn ensure(&mut self, _devices: &[DeviceInfo]) -> Result<DeviceId, VirtualError> {
            lock(&self.state).calls.push("ensure".to_owned());
            Ok(DeviceId::new("virtual"))
        }

        fn release(&mut self) -> Result<(), VirtualError> {
            lock(&self.state).calls.push("release".to_owned());
            Ok(())
        }
    }

    struct FakeFactory {
        state: State,
    }

    impl PipelineFactory for FakeFactory {
        fn start(&self, config: PipelineConfig) -> Result<Box<dyn Pipeline>, PipelineError> {
            let mut shared = lock(&self.state);
            shared
                .calls
                .push(format!("start:{}:{:?}", config.target_id, config.latency));
            if shared.start_failures_left > 0 {
                shared.start_failures_left -= 1;
                shared.calls.push("start_failed".to_owned());
                return Err(PipelineError::Failed("boom".to_owned()));
            }
            Ok(Box::new(FakePipeline {
                state: Arc::clone(&self.state),
            }))
        }
    }

    struct FakePipeline {
        state: State,
    }

    impl Pipeline for FakePipeline {
        fn set_eq(&mut self, eq: EqParams) {
            lock(&self.state).eq_pushes.push(eq);
        }

        fn stats(&self) -> Stats {
            Stats::default()
        }

        fn stop(self: Box<Self>) {
            lock(&self.state).calls.push("stop".to_owned());
        }
    }

    fn test_devices() -> Vec<DeviceInfo> {
        vec![
            DeviceInfo {
                id: DeviceId::new("virtual"),
                name: "CABLE Input".to_owned(),
                is_virtual: true,
            },
            DeviceInfo {
                id: DeviceId::new("speakers"),
                name: "Speakers".to_owned(),
                is_virtual: false,
            },
            DeviceInfo {
                id: DeviceId::new("headphones"),
                name: "Headphones".to_owned(),
                is_virtual: false,
            },
        ]
    }

    /// Short timings so the tests run in well under a second.
    fn test_config() -> ControllerConfig {
        ControllerConfig {
            debounce: Duration::from_millis(30),
            stats_interval: Duration::from_millis(10),
            save_delay: Duration::from_millis(10),
            retry_backoff: vec![
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(30),
            ],
        }
    }

    fn presets_in(dir: &tempfile::TempDir) -> PresetStore {
        PresetStore::open(dir.path().join("presets")).expect("open presets")
    }

    /// Waits for the first event that matches `pred`, skipping the others.
    fn wait_for(events: &Receiver<AppEvent>, pred: impl Fn(&AppEvent) -> bool) -> AppEvent {
        loop {
            let event = events
                .recv_timeout(TIMEOUT)
                .expect("expected event within timeout");
            if pred(&event) {
                return event;
            }
        }
    }

    /// A running controller on its own thread, with the fakes and channels the tests use.
    struct Rig {
        state: State,
        commands: Sender<AppCommand>,
        device_events: Sender<DeviceEvent>,
        events: Receiver<AppEvent>,
        join: Option<JoinHandle<()>>,
        settings_path: PathBuf,
        _dir: tempfile::TempDir,
    }

    impl Rig {
        /// Starts a controller with `settings`; the first `start_failures` pipeline starts fail.
        fn start(settings: Settings, start_failures: usize) -> Self {
            let dir = tempfile::tempdir().expect("temp dir");
            let settings_path = dir.path().join("settings.toml");
            let state: State = Arc::new(Mutex::new(Shared {
                devices: test_devices(),
                default: Some(DeviceId::new("speakers")),
                start_failures_left: start_failures,
                ..Shared::default()
            }));
            let (commands, command_rx) = unbounded();
            let (device_events, device_rx) = unbounded();
            let (events_tx, events) = unbounded();
            let backend_state = Arc::clone(&state);
            let join = Controller::spawn(
                move || Ok(Box::new(FakeBackend::new(backend_state)) as Box<dyn AudioBackend>),
                presets_in(&dir),
                SettingsStore::new(settings_path.clone()),
                settings,
                test_config(),
                events_tx,
                command_rx,
                device_rx,
            )
            .expect("spawn controller thread");
            Self {
                state,
                commands,
                device_events,
                events,
                join: Some(join),
                settings_path,
                _dir: dir,
            }
        }

        fn send(&self, command: AppCommand) {
            self.commands.send(command).expect("controller is running");
        }

        /// Waits for a snapshot that satisfies `pred`.
        fn wait_snapshot(&self, pred: impl Fn(&AppSnapshot) -> bool) -> AppSnapshot {
            let event = wait_for(
                &self.events,
                |event| matches!(event, AppEvent::Snapshot(snapshot) if pred(snapshot)),
            );
            match event {
                AppEvent::Snapshot(snapshot) => snapshot,
                other => panic!("expected a snapshot, got {other:?}"),
            }
        }

        /// Waits for a snapshot whose routing status is `status`.
        fn wait_status(&self, status: RoutingStatus) -> AppSnapshot {
            self.wait_snapshot(|snapshot| snapshot.status == status)
        }

        /// Sends Quit, waits for the controller thread to finish, and returns what the fakes
        /// recorded.
        fn finish(&mut self) -> Outcome {
            self.send(AppCommand::Quit);
            if let Some(join) = self.join.take() {
                join.join().expect("controller thread panicked");
            }
            let shared = lock(&self.state);
            Outcome {
                calls: shared.calls.clone(),
                eq_pushes: shared.eq_pushes.clone(),
            }
        }
    }

    fn powered_on_settings() -> Settings {
        Settings {
            power_on: true,
            ..Settings::default()
        }
    }

    #[test]
    fn power_on_and_off_call_sequence() {
        let mut rig = Rig::start(
            Settings {
                power_on: false,
                ..Settings::default()
            },
            0,
        );
        rig.send(AppCommand::SetPower(true));
        rig.wait_status(RoutingStatus::On);
        rig.send(AppCommand::SetPower(false));
        rig.wait_status(RoutingStatus::Off);
        let outcome = rig.finish();
        assert_eq!(
            outcome.calls,
            [
                "ensure",
                "set_default:virtual",
                "start:speakers:Auto",
                "stop",
                "set_default:speakers",
                "release",
            ]
        );
    }

    #[test]
    fn three_default_changes_in_the_window_cause_one_restart() {
        let mut rig = Rig::start(powered_on_settings(), 0);
        rig.wait_status(RoutingStatus::On);
        for _ in 0..3 {
            rig.device_events
                .send(DeviceEvent::DefaultChanged(DeviceId::new("headphones")))
                .expect("controller is running");
        }
        rig.wait_snapshot(|snapshot| snapshot.target_name.as_deref() == Some("Headphones"));
        let outcome = rig.finish();
        let restarts = outcome
            .calls
            .iter()
            .filter(|call| call.starts_with("start:headphones"))
            .count();
        assert_eq!(restarts, 1);
        assert_eq!(
            outcome.calls.iter().filter(|call| *call == "stop").count(),
            2
        );
    }

    #[test]
    fn editing_a_gain_sets_modified_and_pushes_eq() {
        let mut rig = Rig::start(powered_on_settings(), 0);
        rig.wait_status(RoutingStatus::On);
        rig.send(AppCommand::SetGain { band: 0, db: 3.0 });
        let snapshot = rig.wait_snapshot(|snapshot| snapshot.gains.first() == Some(&3.0));
        assert!(snapshot.modified);
        assert_eq!(snapshot.active_preset.as_deref(), Some("Flat"));
        let outcome = rig.finish();
        let pushed = outcome.eq_pushes.last().expect("EQ was pushed");
        assert_eq!(pushed.band_gains().first(), Some(&3.0));
        assert!(pushed.enabled);
    }

    #[test]
    fn selecting_a_preset_sends_its_gains() {
        let mut rig = Rig::start(powered_on_settings(), 0);
        rig.wait_status(RoutingStatus::On);
        rig.send(AppCommand::SelectPreset("Rock".to_owned()));
        let snapshot =
            rig.wait_snapshot(|snapshot| snapshot.active_preset.as_deref() == Some("Rock"));
        assert!(!snapshot.modified);
        assert_eq!(snapshot.gains, ROCK.to_vec());
        let outcome = rig.finish();
        let pushed = outcome.eq_pushes.last().expect("EQ was pushed");
        assert_eq!(pushed.band_gains(), ROCK.as_slice());
    }

    #[test]
    fn latency_change_restarts_pipeline_with_new_mode() {
        let mut rig = Rig::start(powered_on_settings(), 0);
        rig.wait_status(RoutingStatus::On);
        rig.send(AppCommand::SetLatency(LatencyMode::Reliable));
        rig.wait_snapshot(|snapshot| snapshot.latency == LatencyMode::Reliable);
        let outcome = rig.finish();
        assert_eq!(
            outcome.calls,
            [
                "ensure",
                "set_default:virtual",
                "start:speakers:Auto",
                "stop",
                "start:speakers:Reliable",
                "stop",
                "set_default:speakers",
                "release",
            ]
        );
    }

    #[test]
    fn selecting_a_device_while_on_restarts_on_it() {
        let mut rig = Rig::start(powered_on_settings(), 0);
        rig.wait_status(RoutingStatus::On);
        rig.send(AppCommand::SelectTarget(DeviceId::new("headphones")));
        let snapshot = rig.wait_snapshot(|snapshot| {
            snapshot.target_id == Some(DeviceId::new("headphones"))
                && snapshot.status == RoutingStatus::On
        });
        assert_eq!(snapshot.target_name.as_deref(), Some("Headphones"));
        let names: Vec<&str> = snapshot.devices.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["Speakers", "Headphones"]);
        let outcome = rig.finish();
        assert_eq!(
            outcome.calls,
            [
                "ensure",
                "set_default:virtual",
                "start:speakers:Auto",
                "stop",
                "start:headphones:Auto",
                "set_default:virtual",
                "stop",
                "set_default:headphones",
                "release",
            ]
        );
    }

    #[test]
    fn device_selected_while_off_is_used_at_next_power_on() {
        let mut rig = Rig::start(
            Settings {
                power_on: false,
                ..Settings::default()
            },
            0,
        );
        rig.wait_snapshot(|snapshot| snapshot.devices.len() == 2);
        rig.send(AppCommand::SelectTarget(DeviceId::new("headphones")));
        rig.wait_snapshot(|snapshot| snapshot.target_id == Some(DeviceId::new("headphones")));
        rig.send(AppCommand::SetPower(true));
        rig.wait_status(RoutingStatus::On);
        let outcome = rig.finish();
        assert_eq!(
            outcome.calls,
            [
                "ensure",
                "set_default:virtual",
                "start:headphones:Auto",
                "stop",
                "set_default:headphones",
                "release",
            ]
        );
        let saved = SettingsStore::new(rig.settings_path.clone()).load();
        assert_eq!(saved.target_device_id, Some(DeviceId::new("headphones")));
    }

    #[test]
    fn failing_pipeline_start_is_retried_and_recovers() {
        let mut rig = Rig::start(powered_on_settings(), 2);
        // Stats are only emitted while a pipeline runs, so this means the third start worked.
        wait_for(&rig.events, |event| matches!(event, AppEvent::Stats(_)));
        let outcome = rig.finish();
        assert_eq!(
            outcome.calls,
            [
                "ensure",
                "set_default:virtual",
                "start:speakers:Auto",
                "start_failed",
                "start:speakers:Auto",
                "start_failed",
                "start:speakers:Auto",
                "stop",
                "set_default:speakers",
                "release",
            ]
        );
    }

    #[test]
    fn pipeline_failing_past_the_backoff_degrades_routing() {
        let mut rig = Rig::start(powered_on_settings(), 4);
        rig.wait_status(RoutingStatus::Degraded(DegradedReason::PipelineFailed));
        let outcome = rig.finish();
        let starts = outcome
            .calls
            .iter()
            .filter(|call| call.starts_with("start:"))
            .count();
        assert_eq!(starts, 4);
        assert_eq!(outcome.calls.last().map(String::as_str), Some("release"));
    }

    #[test]
    fn quit_powers_off_and_saves_settings_with_power_still_on() {
        let mut rig = Rig::start(powered_on_settings(), 0);
        rig.wait_status(RoutingStatus::On);
        rig.send(AppCommand::SetLatency(LatencyMode::Low));
        rig.wait_snapshot(|snapshot| snapshot.latency == LatencyMode::Low);
        let outcome = rig.finish();
        assert_eq!(outcome.calls.last().map(String::as_str), Some("release"));
        let saved = SettingsStore::new(rig.settings_path.clone()).load();
        assert!(saved.power_on);
        assert_eq!(saved.latency_mode, LatencyMode::Low);
        assert_eq!(saved.target_device_id, Some(DeviceId::new("speakers")));
    }

    #[test]
    fn run_returns_when_quit_is_already_queued() {
        let dir = tempfile::tempdir().expect("temp dir");
        let settings_path = dir.path().join("settings.toml");
        let state: State = Arc::new(Mutex::new(Shared {
            devices: test_devices(),
            default: Some(DeviceId::new("speakers")),
            ..Shared::default()
        }));
        let (commands, command_rx) = unbounded();
        let (_device_tx, device_rx) = unbounded();
        let (events_tx, events) = unbounded();
        commands.send(AppCommand::Quit).expect("queue quit");
        let controller = Controller::new(
            Box::new(FakeBackend::new(state)),
            presets_in(&dir),
            SettingsStore::new(settings_path.clone()),
            Settings {
                power_on: false,
                ..Settings::default()
            },
            test_config(),
            events_tx,
        );
        controller.run(command_rx, device_rx);
        // The startup snapshot is published before the queued Quit is handled.
        assert!(matches!(events.try_recv(), Ok(AppEvent::Snapshot(_))));
        assert!(settings_path.exists());
    }

    #[test]
    fn spawn_builds_the_backend_on_the_controller_thread_and_quits() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state: State = Arc::new(Mutex::new(Shared {
            devices: test_devices(),
            default: Some(DeviceId::new("speakers")),
            ..Shared::default()
        }));
        let (commands, command_rx) = unbounded();
        let (_device_tx, device_rx) = unbounded();
        let (events_tx, events) = unbounded();
        let factory_state = Arc::clone(&state);
        let join = Controller::spawn(
            move || {
                let thread_name = thread::current().name().unwrap_or_default().to_owned();
                lock(&factory_state)
                    .calls
                    .push(format!("backend:{thread_name}"));
                Ok(Box::new(FakeBackend::new(factory_state)) as Box<dyn AudioBackend>)
            },
            presets_in(&dir),
            SettingsStore::new(dir.path().join("settings.toml")),
            Settings {
                power_on: false,
                ..Settings::default()
            },
            test_config(),
            events_tx,
            command_rx,
            device_rx,
        )
        .expect("spawn controller thread");
        commands
            .send(AppCommand::SetPower(true))
            .expect("controller is running");
        wait_for(
            &events,
            |event| matches!(event, AppEvent::Snapshot(snapshot) if snapshot.status == RoutingStatus::On),
        );
        commands
            .send(AppCommand::Quit)
            .expect("controller is running");
        join.join().expect("controller thread panicked");
        assert_eq!(
            lock(&state).calls.first().map(String::as_str),
            Some("backend:controller")
        );
    }

    #[test]
    fn spawn_reports_a_backend_failure_and_the_thread_ends() {
        let dir = tempfile::tempdir().expect("temp dir");
        let (_commands, command_rx) = unbounded();
        let (_device_tx, device_rx) = unbounded();
        let (events_tx, events) = unbounded();
        let join = Controller::spawn(
            || Err(BackendError::Unavailable("pipewire not running".to_owned())),
            presets_in(&dir),
            SettingsStore::new(dir.path().join("settings.toml")),
            Settings::default(),
            test_config(),
            events_tx,
            command_rx,
            device_rx,
        )
        .expect("spawn controller thread");
        let event = wait_for(&events, |event| matches!(event, AppEvent::Error(_)));
        assert!(
            matches!(&event, AppEvent::Error(message) if message.contains("pipewire not running")),
            "unexpected event {event:?}"
        );
        join.join().expect("thread ends cleanly");
    }
}
