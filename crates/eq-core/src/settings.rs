//! Persisted settings and the locations of the files the app keeps.
//!
//! Settings are a TOML file. A missing file yields defaults; a corrupt file is renamed to
//! `<file>.bak` and defaults are used, so loading never fails and the app always starts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::{DeviceId, LatencyMode};
use crate::presets::write_atomic;

/// Current version of the settings file format.
pub const SETTINGS_VERSION: u32 = 1;

/// The user's persisted settings.
///
/// Missing fields take their default value and unknown fields are ignored, so an older or newer
/// file still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Format version of the file.
    pub version: u32,
    /// Whether processing was on when the app last ran.
    pub power_on: bool,
    /// Name of the preset that was selected, if any.
    // Absent means "no active preset", not the default preset: TOML cannot store `None`, so
    // omitting the key is the only way to round-trip it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_preset: Option<String>,
    /// Band gains in dB at the last change.
    pub current_gains: Vec<f32>,
    /// Latency mode the user selected.
    pub latency_mode: LatencyMode,
    /// Target output device chosen by the router, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_device_id: Option<DeviceId>,
    /// Name pattern that locates the virtual device on Windows (VB-CABLE).
    pub virtual_device_pattern: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            power_on: true,
            active_preset: Some("Flat".to_owned()),
            current_gains: vec![0.0; 10],
            latency_mode: LatencyMode::Auto,
            target_device_id: None,
            virtual_device_pattern: "CABLE Input".to_owned(),
        }
    }
}

/// Errors returned by [`SettingsStore::save`].
#[derive(Debug, Error)]
pub enum SettingsError {
    /// Writing the settings file failed.
    #[error("settings file operation failed: {0}")]
    Io(#[from] io::Error),
    /// The settings could not be encoded as TOML.
    #[error("settings could not be encoded: {0}")]
    Encode(#[from] toml::ser::Error),
}

/// Loads and saves [`Settings`] at one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Creates a store for the settings file at `path`. The file need not exist yet.
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Returns the settings file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads the settings. Never fails.
    ///
    /// A missing file gives defaults. A file that cannot be parsed (or is not valid UTF-8) is
    /// renamed to `<file>.bak`, replacing an older backup, and defaults are returned.
    #[must_use]
    pub fn load(&self) -> Settings {
        match fs::read_to_string(&self.path) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(settings) => settings,
                Err(_) => self.quarantine_and_default(),
            },
            Err(err) if err.kind() == io::ErrorKind::InvalidData => self.quarantine_and_default(),
            // A missing or unreadable file is not corrupt; the defaults are used until the next save.
            Err(_) => Settings::default(),
        }
    }

    /// Writes the settings atomically (temporary file, then rename), creating the directory
    /// if needed.
    ///
    /// # Errors
    ///
    /// Returns [`SettingsError::Encode`] if the settings cannot be encoded, and
    /// [`SettingsError::Io`] if the directory or file cannot be written.
    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        let text = toml::to_string_pretty(settings)?;
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&self.path, text.as_bytes())?;
        Ok(())
    }

    /// Moves the unreadable file aside and returns the defaults.
    fn quarantine_and_default(&self) -> Settings {
        let mut backup = self.path.as_os_str().to_owned();
        backup.push(".bak");
        // Best effort: if the rename fails the app still starts with defaults, and the next
        // save overwrites the broken file anyway.
        let _ = fs::rename(&self.path, PathBuf::from(backup));
        Settings::default()
    }
}

/// Locations of the configuration, presets, settings file and logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Directory that holds the settings file and the presets directory.
    pub config_dir: PathBuf,
    /// Directory with one JSON file per user preset.
    pub presets_dir: PathBuf,
    /// The TOML settings file.
    pub settings_file: PathBuf,
    /// Directory for log files.
    pub log_dir: PathBuf,
}

impl AppPaths {
    /// Builds all paths below `root`, for tests and portable use.
    #[must_use]
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            presets_dir: root.join("presets"),
            settings_file: root.join("settings.toml"),
            log_dir: root.join("logs"),
            config_dir: root,
        }
    }

    /// Returns the per-user locations from the `directories` crate, or `None` when the
    /// platform has no home directory.
    ///
    /// Logs go to the state directory where the platform has one (Linux), otherwise to the
    /// local data directory (Windows).
    #[must_use]
    pub fn discover() -> Option<Self> {
        let dirs = ProjectDirs::from("", "", "equalizerer")?;
        let config_dir = dirs.config_dir().to_path_buf();
        let state_dir = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
        Some(Self {
            presets_dir: config_dir.join("presets"),
            settings_file: config_dir.join("settings.toml"),
            log_dir: state_dir.join("logs"),
            config_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(dir: &tempfile::TempDir) -> SettingsStore {
        SettingsStore::new(dir.path().join("settings.toml"))
    }

    #[test]
    fn defaults_match_architecture() {
        let settings = Settings::default();
        assert_eq!(settings.version, 1);
        assert!(settings.power_on);
        assert_eq!(settings.active_preset.as_deref(), Some("Flat"));
        assert_eq!(settings.current_gains, vec![0.0; 10]);
        assert_eq!(settings.latency_mode, LatencyMode::Auto);
        assert_eq!(settings.target_device_id, None);
        assert_eq!(settings.virtual_device_pattern, "CABLE Input");
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        let settings = Settings {
            version: 1,
            power_on: false,
            active_preset: Some("Rock".to_owned()),
            current_gains: vec![4.0, 3.0, 1.0, -1.0, -2.0, -1.0, 1.0, 3.0, 4.0, 4.0],
            latency_mode: LatencyMode::Reliable,
            target_device_id: Some(DeviceId::new("{0.0.0.00000000}.{abc}")),
            virtual_device_pattern: "CABLE Input".to_owned(),
        };
        store.save(&settings).expect("save");
        assert_eq!(store.load(), settings);
    }

    #[test]
    fn absent_optional_fields_round_trip_as_none() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        let settings = Settings {
            active_preset: None,
            target_device_id: None,
            ..Settings::default()
        };
        store.save(&settings).expect("save");
        let text = fs::read_to_string(store.path()).expect("read");
        assert!(!text.contains("target_device_id"));
        assert!(!text.contains("active_preset"));
        assert_eq!(store.load(), settings);
    }

    #[test]
    fn latency_mode_is_written_in_lowercase() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        store
            .save(&Settings {
                latency_mode: LatencyMode::Low,
                ..Settings::default()
            })
            .expect("save");
        let text = fs::read_to_string(store.path()).expect("read");
        assert!(text.contains("latency_mode = \"low\""));
    }

    #[test]
    fn missing_file_gives_defaults_without_creating_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        assert_eq!(store.load(), Settings::default());
        assert!(!store.path().exists());
    }

    #[test]
    fn corrupt_file_is_moved_to_bak_and_defaults_are_used() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        fs::write(store.path(), "power_on = [unclosed").expect("write");
        assert_eq!(store.load(), Settings::default());
        assert!(!store.path().exists());
        let bak = dir.path().join("settings.toml.bak");
        assert_eq!(
            fs::read_to_string(&bak).expect("backup"),
            "power_on = [unclosed"
        );
    }

    #[test]
    fn second_corruption_replaces_the_older_backup() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        fs::write(store.path(), "first = [").expect("write");
        let _ = store.load();
        fs::write(store.path(), "second = [").expect("write");
        let _ = store.load();
        let bak = dir.path().join("settings.toml.bak");
        assert_eq!(fs::read_to_string(bak).expect("backup"), "second = [");
    }

    #[test]
    fn wrong_field_type_counts_as_corrupt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        fs::write(store.path(), "power_on = \"yes\"\n").expect("write");
        assert_eq!(store.load(), Settings::default());
        assert!(dir.path().join("settings.toml.bak").exists());
    }

    #[test]
    fn unknown_fields_are_ignored_and_missing_fields_default() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        fs::write(
            store.path(),
            "version = 1\npower_on = false\nfuture_feature = true\n\n[extra]\nvalue = 3\n",
        )
        .expect("write");
        let settings = store.load();
        assert!(!settings.power_on);
        assert_eq!(settings.virtual_device_pattern, "CABLE Input");
        assert_eq!(settings.current_gains, vec![0.0; 10]);
        assert!(!dir.path().join("settings.toml.bak").exists());
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = SettingsStore::new(dir.path().join("nested").join("settings.toml"));
        store.save(&Settings::default()).expect("save");
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn save_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = temp_store(&dir);
        store.save(&Settings::default()).expect("save");
        assert!(!dir.path().join("settings.toml.tmp").exists());
    }

    #[test]
    fn from_root_places_everything_below_root() {
        let paths = AppPaths::from_root("/data/eq");
        assert_eq!(paths.config_dir, PathBuf::from("/data/eq"));
        assert_eq!(paths.presets_dir, PathBuf::from("/data/eq").join("presets"));
        assert_eq!(
            paths.settings_file,
            PathBuf::from("/data/eq").join("settings.toml")
        );
        assert_eq!(paths.log_dir, PathBuf::from("/data/eq").join("logs"));
    }

    #[test]
    fn discover_uses_the_app_name_when_a_home_exists() {
        if let Some(paths) = AppPaths::discover() {
            assert!(paths.settings_file.ends_with("settings.toml"));
            assert!(paths.presets_dir.ends_with("presets"));
            assert!(paths.log_dir.ends_with("logs"));
        }
    }
}
