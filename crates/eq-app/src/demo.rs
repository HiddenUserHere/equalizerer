//! Demo controller for the `--demo` UI development mode. It is not used in normal runs.
//!
//! It stands in for the real controller so the window can be exercised without touching audio
//! devices or settings. Keeps a local copy of the state, answers the commands the UI can send and publishes a new
//! snapshot after each change. It has no audio side effects. While power is on it also emits a
//! fake [`Stats`] event every 250 ms, so that the latency label and the dropout hint can be
//! exercised.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use eq_core::{
    AppCommand, AppEvent, AppSnapshot, LatencyMode, PresetSummary, RoutingStatus, Stats,
};
use eq_dsp::{GAIN_MAX_DB, GAIN_MIN_DB, Layout};

use crate::ui::presets_view::validate_name;

/// Interval between fake statistics events while power is on.
const STATS_INTERVAL: Duration = Duration::from_millis(250);

/// Starts the demo controller on its own thread.
///
/// It publishes the initial snapshot immediately and exits on [`AppCommand::Quit`] or when the
/// command channel closes.
pub fn spawn(commands: Receiver<AppCommand>, events: Sender<AppEvent>) -> JoinHandle<()> {
    std::thread::spawn(move || run(&commands, &events))
}

/// A preset held by the demo controller.
#[derive(Debug, Clone, PartialEq)]
struct DemoPreset {
    name: String,
    builtin: bool,
    gains: Vec<f32>,
}

/// State of the demo controller.
struct DemoState {
    power: bool,
    latency: LatencyMode,
    /// Built-in presets first, then user presets sorted case-insensitively.
    presets: Vec<DemoPreset>,
    /// Name of the preset the gains were loaded from.
    active: Option<String>,
    /// `true` when the gains differ from the active preset.
    modified: bool,
    gains: Vec<f32>,
}

impl DemoState {
    /// Power off, Flat selected, three built-in presets with the values of `eq-core`.
    fn initial() -> Self {
        let flat = vec![0.0; Layout::Ten.len()];
        let presets = vec![
            builtin("Flat", flat.clone()),
            builtin(
                "Bass Boost",
                vec![6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ),
            builtin(
                "Rock",
                vec![4.0, 3.0, 1.0, -1.0, -2.0, -1.0, 1.0, 3.0, 4.0, 4.0],
            ),
        ];
        Self {
            power: false,
            latency: LatencyMode::Auto,
            presets,
            active: Some("Flat".to_owned()),
            modified: false,
            gains: flat,
        }
    }

    /// Applies one command. `Err` carries the message shown to the user; nothing changes then.
    fn handle(&mut self, command: AppCommand) -> Result<(), String> {
        match command {
            AppCommand::SetPower(on) => self.power = on,
            AppCommand::SetGain { band, db } => {
                if let Some(gain) = self.gains.get_mut(band) {
                    *gain = db.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
                    self.touch();
                }
            }
            AppCommand::SetGains(gains) => {
                if gains.len() == self.gains.len() {
                    self.gains = gains
                        .into_iter()
                        .map(|db| db.clamp(GAIN_MIN_DB, GAIN_MAX_DB))
                        .collect();
                    self.touch();
                }
            }
            AppCommand::SelectPreset(name) => self.select(&name)?,
            AppCommand::SavePreset => self.save()?,
            AppCommand::SavePresetAs(name) => self.save_as(&name)?,
            AppCommand::RenamePreset { from, to } => self.rename(&from, &to)?,
            AppCommand::DeletePreset(name) => self.delete(&name)?,
            AppCommand::SetLatency(mode) => self.latency = mode,
            AppCommand::ShowWindow | AppCommand::Quit => {
                return Err(format!("Not available in the demo controller: {command:?}"));
            }
        }
        Ok(())
    }

    /// Marks the gains as edited. Without an active preset there is nothing to be modified from.
    fn touch(&mut self) {
        self.modified = self.active.is_some();
    }

    /// Current preset list in the shape the UI expects.
    fn summaries(&self) -> Vec<PresetSummary> {
        self.presets
            .iter()
            .map(|preset| PresetSummary {
                name: preset.name.clone(),
                builtin: preset.builtin,
            })
            .collect()
    }

    /// Copy of the state for the UI.
    fn snapshot(&self) -> AppSnapshot {
        AppSnapshot {
            power: self.power,
            status: if self.power {
                RoutingStatus::On
            } else {
                RoutingStatus::Off
            },
            target_name: Some("Demo output".to_owned()),
            presets: self.summaries(),
            active_preset: self.active.clone(),
            modified: self.modified,
            gains: self.gains.clone(),
            latency: self.latency,
        }
    }

    /// Fake statistics for the current latency mode, with no underruns.
    fn stats(&self) -> Stats {
        let latency_ms = match self.latency {
            LatencyMode::Low => 12.0,
            LatencyMode::Auto => 40.0,
            LatencyMode::Reliable => 90.0,
        };
        Stats {
            latency_ms,
            underruns: 0,
            resyncs: 0,
        }
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.presets.iter().position(|preset| preset.name == name)
    }

    fn select(&mut self, name: &str) -> Result<(), String> {
        let index = self
            .find(name)
            .ok_or_else(|| format!("No preset named \u{201c}{name}\u{201d}."))?;
        self.gains = self.presets[index].gains.clone();
        self.active = Some(self.presets[index].name.clone());
        self.modified = false;
        Ok(())
    }

    fn save(&mut self) -> Result<(), String> {
        let active = self.active.clone().ok_or("No preset is selected.")?;
        let index = self
            .find(&active)
            .ok_or("The active preset no longer exists.")?;
        if self.presets[index].builtin {
            return Err("Built-in presets cannot be overwritten. Use Save as\u{2026}".to_owned());
        }
        if self.modified {
            self.presets[index].gains.clone_from(&self.gains);
            self.modified = false;
        }
        Ok(())
    }

    fn save_as(&mut self, name: &str) -> Result<(), String> {
        let name =
            validate_name(name, &self.summaries(), None).map_err(|error| message_of(&error))?;
        self.insert_user(DemoPreset {
            name: name.clone(),
            builtin: false,
            gains: self.gains.clone(),
        });
        self.active = Some(name);
        self.modified = false;
        Ok(())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let index = self
            .find(from)
            .ok_or_else(|| format!("No preset named \u{201c}{from}\u{201d}."))?;
        if self.presets[index].builtin {
            return Err("Built-in presets cannot be renamed.".to_owned());
        }
        let to = match validate_name(to, &self.summaries(), Some(from)) {
            Ok(name) => name,
            // Renaming to the same name changes nothing.
            Err(presets_error) if presets_error.message().is_none() => return Ok(()),
            Err(presets_error) => return Err(message_of(&presets_error)),
        };
        let mut preset = self.presets.remove(index);
        if self.active.as_deref() == Some(from) {
            self.active = Some(to.clone());
        }
        preset.name = to;
        self.insert_user(preset);
        Ok(())
    }

    fn delete(&mut self, name: &str) -> Result<(), String> {
        let index = self
            .find(name)
            .ok_or_else(|| format!("No preset named \u{201c}{name}\u{201d}."))?;
        if self.presets[index].builtin {
            return Err("Built-in presets cannot be deleted.".to_owned());
        }
        self.presets.remove(index);
        if self.active.as_deref() == Some(name) {
            self.active = None;
            self.modified = false;
        }
        Ok(())
    }

    /// Inserts a user preset after the built-ins, keeping the user part sorted.
    fn insert_user(&mut self, preset: DemoPreset) {
        let lowered = preset.name.to_lowercase();
        let position = self
            .presets
            .iter()
            .position(|other| !other.builtin && other.name.to_lowercase() > lowered)
            .unwrap_or(self.presets.len());
        self.presets.insert(position, preset);
    }
}

/// A built-in preset.
fn builtin(name: &str, gains: Vec<f32>) -> DemoPreset {
    DemoPreset {
        name: name.to_owned(),
        builtin: true,
        gains,
    }
}

/// Text for a name error. Errors without a message never reach here for save or rename.
fn message_of(error: &crate::ui::presets_view::NameError) -> String {
    error
        .message()
        .unwrap_or_else(|| "The name is not valid.".to_owned())
}

/// Main loop of the demo controller.
///
/// The statistics deadline is kept separately from the command wait, so a stream of commands
/// does not delay the fake statistics.
fn run(commands: &Receiver<AppCommand>, events: &Sender<AppEvent>) {
    let mut state = DemoState::initial();
    publish(events, &state);

    let mut next_stats = Instant::now() + STATS_INTERVAL;
    loop {
        let wait = next_stats.saturating_duration_since(Instant::now());
        match commands.recv_timeout(wait) {
            Ok(AppCommand::Quit) | Err(RecvTimeoutError::Disconnected) => break,
            Ok(command) => match state.handle(command) {
                Ok(()) => publish(events, &state),
                Err(message) => {
                    let _ = events.send(AppEvent::Error(message));
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
        }

        if Instant::now() >= next_stats {
            next_stats = Instant::now() + STATS_INTERVAL;
            if state.power {
                let _ = events.send(AppEvent::Stats(state.stats()));
            }
        }
    }
}

/// Sends a copy of the current state to the UI.
fn publish(events: &Sender<AppEvent>, state: &DemoState) {
    let _ = events.send(AppEvent::Snapshot(state.snapshot()));
}
