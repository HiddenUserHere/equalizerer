//! Built-in and user presets.
//!
//! Built-in presets are compiled in and read-only. User presets are stored as one JSON file per
//! preset in the presets directory, and every write goes through a temporary file followed by a
//! rename, so a crash never leaves a half-written preset behind.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use eq_dsp::{GAIN_MAX_DB, GAIN_MIN_DB, Layout};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::messages::PresetSummary;
use crate::model::LayoutId;

/// Longest allowed preset name, in characters.
const MAX_NAME_CHARS: usize = 64;

/// Characters that cannot appear in a file name on Windows (and are rejected everywhere, so
/// that a preset file name is identical on every OS).
const FORBIDDEN_NAME_CHARS: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// Device names that Windows reserves; they are rejected with any extension.
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// The built-in presets in display order, with 10-band gains in dB (32 Hz to 16 kHz).
const BUILTIN_PRESETS: [(&str, [f32; 10]); 9] = [
    ("Flat", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    (
        "Bass Boost",
        [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    (
        "Bass Reducer",
        [-6.0, -5.0, -4.0, -2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
    ),
    (
        "Treble Boost",
        [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 4.0, 5.0, 6.0],
    ),
    (
        "Vocal",
        [-2.0, -2.0, -1.0, 1.0, 3.0, 4.0, 3.0, 1.0, 0.0, -1.0],
    ),
    (
        "Rock",
        [4.0, 3.0, 1.0, -1.0, -2.0, -1.0, 1.0, 3.0, 4.0, 4.0],
    ),
    (
        "Electronic",
        [5.0, 4.0, 1.0, 0.0, -2.0, 1.0, 0.0, 2.0, 4.0, 5.0],
    ),
    (
        "Acoustic",
        [3.0, 2.0, 1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 2.0, 1.0],
    ),
    (
        "Loudness",
        [6.0, 4.0, 0.0, 0.0, -2.0, 0.0, -1.0, 2.0, 5.0, 6.0],
    ),
];

/// A named EQ curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    /// Name, unique case-insensitively across built-in and user presets.
    pub name: String,
    /// Band layout the gains refer to.
    pub layout: LayoutId,
    /// One gain in dB per band of `layout`.
    pub gains: Vec<f32>,
    /// `true` for built-in presets. Never written to disk; always `false` when loaded.
    #[serde(skip)]
    pub builtin: bool,
}

/// Errors returned by [`PresetStore`] and by name and gain validation.
#[derive(Debug, Error)]
pub enum PresetError {
    /// The name is not usable as a preset name and file name.
    #[error("invalid preset name `{name}`: {reason}")]
    InvalidName {
        /// The name as given by the caller.
        name: String,
        /// Why it was rejected.
        reason: &'static str,
    },
    /// The operation would change a built-in preset.
    #[error("`{0}` is a built-in preset and cannot be changed")]
    BuiltIn(String),
    /// Another preset already uses this name (case-insensitively).
    #[error("a preset named `{0}` already exists")]
    Duplicate(String),
    /// No user preset has this name.
    #[error("no user preset named `{0}`")]
    NotFound(String),
    /// The number of gains does not match the layout.
    #[error("preset has {actual} gains, the layout needs {expected}")]
    GainCount {
        /// Number of bands in the layout.
        expected: usize,
        /// Number of gains supplied.
        actual: usize,
    },
    /// A gain is NaN or infinite.
    #[error("preset gains must be finite numbers")]
    NonFiniteGain,
    /// Reading or writing a preset file failed.
    #[error("preset file operation failed: {0}")]
    Io(#[from] io::Error),
    /// A preset could not be converted to or from JSON.
    #[error("preset JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

/// Checks a preset name and returns it trimmed.
///
/// The name becomes a file name (`<name>.json`), so it must be valid on Windows and Linux alike.
///
/// # Errors
///
/// Returns [`PresetError::InvalidName`] when the name is empty, longer than 64 characters,
/// contains a forbidden or control character, ends with a dot, or is a Windows reserved name
/// (also with an extension, for example `nul.txt`).
pub fn validate_name(name: &str) -> Result<String, PresetError> {
    let reject = |reason: &'static str| PresetError::InvalidName {
        name: name.to_owned(),
        reason,
    };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(reject("the name is empty"));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(reject("the name is longer than 64 characters"));
    }
    if trimmed.chars().any(|c| FORBIDDEN_NAME_CHARS.contains(&c)) {
        return Err(reject(
            "the name contains a character not allowed in file names",
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(reject("the name contains a control character"));
    }
    if trimmed.ends_with('.') {
        return Err(reject("the name ends with a dot"));
    }
    // Windows ignores everything after the first dot when checking reserved names, and a
    // trailing space before that dot does not change the reserved name either.
    let base = trimmed.split('.').next().unwrap_or(trimmed).trim_end();
    let base_upper = base.to_ascii_uppercase();
    if RESERVED_NAMES.contains(&base_upper.as_str()) {
        return Err(reject("the name is a reserved Windows file name"));
    }
    Ok(trimmed.to_owned())
}

/// Checks a gain vector against a layout and clamps every gain to the allowed range.
///
/// # Errors
///
/// Returns [`PresetError::GainCount`] when the length does not match the layout, and
/// [`PresetError::NonFiniteGain`] when a gain is NaN or infinite.
pub fn validate_gains(layout: LayoutId, mut gains: Vec<f32>) -> Result<Vec<f32>, PresetError> {
    let expected = Layout::from(layout).len();
    if gains.len() != expected {
        return Err(PresetError::GainCount {
            expected,
            actual: gains.len(),
        });
    }
    if gains.iter().any(|gain| !gain.is_finite()) {
        return Err(PresetError::NonFiniteGain);
    }
    for gain in &mut gains {
        *gain = gain.clamp(GAIN_MIN_DB, GAIN_MAX_DB);
    }
    Ok(gains)
}

/// Returns the built-in presets in display order.
fn builtin_presets() -> Vec<Preset> {
    BUILTIN_PRESETS
        .iter()
        .map(|(name, gains)| Preset {
            name: (*name).to_owned(),
            layout: LayoutId::Ten,
            gains: gains.to_vec(),
            builtin: true,
        })
        .collect()
}

/// Names are compared case-insensitively, because Windows file names are.
fn same_name(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Writes `bytes` to `path` through `<path>.tmp` and a rename, so readers never see a partial
/// file. Shared with the settings store.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)
}

/// Removes a file, treating "already gone" as success.
fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// A user preset together with the file it was loaded from or written to.
#[derive(Debug)]
struct Entry {
    preset: Preset,
    path: PathBuf,
}

/// Built-in and user presets, backed by a directory with one JSON file per user preset.
///
/// The store is not thread-safe by itself; the controller owns it.
#[derive(Debug)]
pub struct PresetStore {
    dir: PathBuf,
    builtins: Vec<Preset>,
    /// User presets sorted case-insensitively by name.
    user: Vec<Entry>,
    load_errors: Vec<String>,
}

impl PresetStore {
    /// Opens the store in `dir`, creating the directory if needed.
    ///
    /// Invalid preset files are skipped and described by [`PresetStore::load_errors`]; they do
    /// not make the open fail, so one damaged file cannot hide the others.
    ///
    /// # Errors
    ///
    /// Returns [`PresetError::Io`] when the directory cannot be created or read.
    pub fn open(dir: PathBuf) -> Result<Self, PresetError> {
        fs::create_dir_all(&dir)?;
        let mut store = Self {
            dir,
            builtins: builtin_presets(),
            user: Vec::new(),
            load_errors: Vec::new(),
        };
        for entry in fs::read_dir(&store.dir)? {
            let path = entry?.path();
            if path.is_dir() || path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let file_name = path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            match load_file(&path) {
                Ok(preset) => {
                    if store.is_builtin(&preset.name) || store.find_user(&preset.name).is_some() {
                        store.load_errors.push(format!(
                            "{file_name}: a preset named `{}` already exists",
                            preset.name
                        ));
                    } else {
                        store.user.push(Entry { preset, path });
                    }
                }
                Err(err) => store.load_errors.push(format!("{file_name}: {err}")),
            }
        }
        store.sort_user();
        Ok(store)
    }

    /// Lists all presets: built-in first in their fixed order, then user presets sorted
    /// case-insensitively.
    #[must_use]
    pub fn list(&self) -> Vec<PresetSummary> {
        self.builtins
            .iter()
            .chain(self.user.iter().map(|entry| &entry.preset))
            .map(|preset| PresetSummary {
                name: preset.name.clone(),
                builtin: preset.builtin,
            })
            .collect()
    }

    /// Returns the preset with this name (built-in or user), matched case-insensitively.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Preset> {
        let name = name.trim();
        self.builtins
            .iter()
            .find(|preset| same_name(&preset.name, name))
            .or_else(|| self.find_user(name))
            .cloned()
    }

    /// Creates a user preset, or overwrites the user preset with the same name.
    ///
    /// Gains are clamped to the allowed range before they are stored.
    ///
    /// # Errors
    ///
    /// Returns [`PresetError::BuiltIn`] for a built-in name, [`PresetError::InvalidName`] or
    /// [`PresetError::GainCount`] for invalid input, and [`PresetError::Io`] when the file
    /// cannot be written.
    pub fn save(&mut self, preset: Preset) -> Result<(), PresetError> {
        let Preset {
            name,
            layout,
            gains,
            ..
        } = preset;
        let name = validate_name(&name)?;
        if self.is_builtin(&name) {
            return Err(PresetError::BuiltIn(name));
        }
        let stored = Preset {
            name,
            layout,
            gains: validate_gains(layout, gains)?,
            builtin: false,
        };
        if let Some(index) = self.user_index(&stored.name) {
            let old = &self.user[index];
            let path = self.persist(Some((&old.preset.name, &old.path)), &stored)?;
            self.user[index] = Entry {
                preset: stored,
                path,
            };
        } else {
            let path = self.persist(None, &stored)?;
            self.user.push(Entry {
                preset: stored,
                path,
            });
            self.sort_user();
        }
        Ok(())
    }

    /// Renames a user preset. The new name must be unique (case-insensitively), but changing
    /// only the case of the same name is allowed.
    ///
    /// # Errors
    ///
    /// Returns [`PresetError::BuiltIn`] when `from` is built-in, [`PresetError::NotFound`] when
    /// no user preset has the name `from`, [`PresetError::Duplicate`] when `to` is taken by
    /// another preset, and [`PresetError::InvalidName`] when `to` is not a valid name.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), PresetError> {
        let from = from.trim();
        let to = validate_name(to)?;
        if self.is_builtin(from) {
            return Err(PresetError::BuiltIn(from.to_owned()));
        }
        if self.is_builtin(&to) {
            return Err(PresetError::Duplicate(to));
        }
        let index = self
            .user_index(from)
            .ok_or_else(|| PresetError::NotFound(from.to_owned()))?;
        let taken_by_other = self
            .user
            .iter()
            .enumerate()
            .any(|(i, entry)| i != index && same_name(&entry.preset.name, &to));
        if taken_by_other {
            return Err(PresetError::Duplicate(to));
        }

        let mut renamed = self.user[index].preset.clone();
        renamed.name.clone_from(&to);
        let old = &self.user[index];
        let path = self.persist(Some((&old.preset.name, &old.path)), &renamed)?;
        self.user[index] = Entry {
            preset: renamed,
            path,
        };
        self.sort_user();
        Ok(())
    }

    /// Deletes a user preset and its file.
    ///
    /// # Errors
    ///
    /// Returns [`PresetError::BuiltIn`] for a built-in name, [`PresetError::NotFound`] when no
    /// user preset has the name, and [`PresetError::Io`] when the file cannot be removed.
    pub fn delete(&mut self, name: &str) -> Result<(), PresetError> {
        let name = name.trim();
        if self.is_builtin(name) {
            return Err(PresetError::BuiltIn(name.to_owned()));
        }
        let index = self
            .user_index(name)
            .ok_or_else(|| PresetError::NotFound(name.to_owned()))?;
        remove_if_exists(&self.user[index].path)?;
        self.user.remove(index);
        Ok(())
    }

    /// Returns the problems found while opening the store, one message per skipped file.
    #[must_use]
    pub fn load_errors(&self) -> &[String] {
        &self.load_errors
    }

    /// Writes a user preset and returns the path it now lives at.
    ///
    /// `old` is the file the preset was stored in before, if any. Its file is removed after
    /// the new one is in place, except for a case-only rename: on a case-insensitive file
    /// system the old and new names are the same file, so the old one is removed first.
    fn persist(&self, old: Option<(&str, &Path)>, preset: &Preset) -> Result<PathBuf, PresetError> {
        let json = serde_json::to_vec_pretty(preset)?;
        let path = self.dir.join(format!("{}.json", preset.name));
        match old {
            None => write_atomic(&path, &json)?,
            Some((old_name, old_path)) if old_name == preset.name => {
                write_atomic(&path, &json)?;
                if old_path != path.as_path() {
                    remove_if_exists(old_path)?;
                }
            }
            Some((old_name, old_path)) if same_name(old_name, &preset.name) => {
                remove_if_exists(old_path)?;
                write_atomic(&path, &json)?;
            }
            Some((_, old_path)) => {
                write_atomic(&path, &json)?;
                remove_if_exists(old_path)?;
            }
        }
        Ok(path)
    }

    fn is_builtin(&self, name: &str) -> bool {
        self.builtins
            .iter()
            .any(|preset| same_name(&preset.name, name))
    }

    fn user_index(&self, name: &str) -> Option<usize> {
        self.user
            .iter()
            .position(|entry| same_name(&entry.preset.name, name))
    }

    fn find_user(&self, name: &str) -> Option<&Preset> {
        self.user_index(name).map(|index| &self.user[index].preset)
    }

    fn sort_user(&mut self) {
        self.user
            .sort_by_cached_key(|entry| entry.preset.name.to_lowercase());
    }
}

/// Reads and validates one preset file.
fn load_file(path: &Path) -> Result<Preset, PresetError> {
    let text = fs::read_to_string(path)?;
    let raw: Preset = serde_json::from_str(&text)?;
    Ok(Preset {
        name: validate_name(&raw.name)?,
        layout: raw.layout,
        gains: validate_gains(raw.layout, raw.gains)?,
        builtin: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, gains: Vec<f32>) -> Preset {
        Preset {
            name: name.to_owned(),
            layout: LayoutId::Ten,
            gains,
            builtin: false,
        }
    }

    fn flat_gains() -> Vec<f32> {
        vec![0.0; 10]
    }

    fn open_in(dir: &tempfile::TempDir) -> PresetStore {
        PresetStore::open(dir.path().join("presets")).expect("open store")
    }

    fn user_names(store: &PresetStore) -> Vec<String> {
        store
            .list()
            .into_iter()
            .filter(|summary| !summary.builtin)
            .map(|summary| summary.name)
            .collect()
    }

    #[test]
    fn builtins_are_listed_first_in_fixed_order_with_exact_values() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = open_in(&dir);
        let names: Vec<String> = store.list().into_iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                "Flat",
                "Bass Boost",
                "Bass Reducer",
                "Treble Boost",
                "Vocal",
                "Rock",
                "Electronic",
                "Acoustic",
                "Loudness",
            ]
        );
        assert!(store.list().iter().all(|summary| summary.builtin));
        assert_eq!(
            store.get("Bass Boost").expect("built-in").gains,
            vec![6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        assert_eq!(
            store.get("loudness").expect("case-insensitive").gains,
            vec![6.0, 4.0, 0.0, 0.0, -2.0, 0.0, -1.0, 2.0, 5.0, 6.0]
        );
        assert_eq!(store.load_errors().len(), 0);
    }

    #[test]
    fn user_presets_are_listed_after_builtins_sorted_case_insensitively() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        store.save(preset("beta", flat_gains())).expect("save");
        store.save(preset("Alpha", flat_gains())).expect("save");
        store.save(preset("gamma", flat_gains())).expect("save");
        assert_eq!(user_names(&store), ["Alpha", "beta", "gamma"]);
        assert_eq!(store.list().len(), 12);
    }

    #[test]
    fn crud_round_trip_survives_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        {
            let mut store = open_in(&dir);
            store.save(preset("Night", vec![1.0; 10])).expect("save");
            assert_eq!(store.get("Night").expect("saved").gains, vec![1.0; 10]);
            store.rename("Night", "Late Night").expect("rename");
            assert!(store.get("Night").is_none());
            store.save(preset("Other", flat_gains())).expect("save");
            store.delete("Other").expect("delete");
        }
        let store = open_in(&dir);
        assert_eq!(user_names(&store), ["Late Night"]);
        assert_eq!(
            store.get("Late Night").expect("reloaded").gains,
            vec![1.0; 10]
        );
        assert_eq!(store.load_errors().len(), 0);
    }

    #[test]
    fn save_overwrites_user_preset_with_same_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        store.save(preset("Mine", flat_gains())).expect("save");
        store
            .save(preset("mine", vec![2.0; 10]))
            .expect("overwrite");
        assert_eq!(user_names(&store), ["mine"]);
        assert_eq!(store.get("Mine").expect("present").gains, vec![2.0; 10]);
    }

    #[test]
    fn rename_to_existing_name_is_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        store.save(preset("A", flat_gains())).expect("save");
        store.save(preset("B", flat_gains())).expect("save");
        assert!(matches!(
            store.rename("A", "b"),
            Err(PresetError::Duplicate(_))
        ));
        assert!(matches!(
            store.rename("missing", "C"),
            Err(PresetError::NotFound(_))
        ));
        assert_eq!(user_names(&store), ["A", "B"]);
    }

    #[test]
    fn case_only_rename_keeps_one_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        store.save(preset("mix", flat_gains())).expect("save");
        store.rename("mix", "MIX").expect("case-only rename");
        assert_eq!(user_names(&store), ["MIX"]);
        let json_files = fs::read_dir(dir.path().join("presets"))
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(json_files, 1);
        assert_eq!(open_in(&dir).load_errors().len(), 0);
    }

    #[test]
    fn built_in_presets_are_protected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        assert!(matches!(
            store.save(preset("rock", flat_gains())),
            Err(PresetError::BuiltIn(_))
        ));
        assert!(matches!(
            store.rename("Flat", "Mine"),
            Err(PresetError::BuiltIn(_))
        ));
        assert!(matches!(
            store.rename("Mine", "Rock"),
            Err(PresetError::Duplicate(_))
        ));
        assert!(matches!(store.delete("Flat"), Err(PresetError::BuiltIn(_))));
        assert_eq!(store.list().len(), 9);
    }

    #[test]
    fn invalid_json_file_is_skipped_with_error_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let presets = dir.path().join("presets");
        fs::create_dir_all(&presets).expect("create dir");
        fs::write(presets.join("broken.json"), "{ not json").expect("write");
        fs::write(
            presets.join("good.json"),
            r#"{"name":"Good","layout":"ten","gains":[1,1,1,1,1,1,1,1,1,1]}"#,
        )
        .expect("write");
        let store = PresetStore::open(presets).expect("open");
        assert_eq!(user_names(&store), ["Good"]);
        assert_eq!(store.load_errors().len(), 1);
        assert!(store.load_errors()[0].starts_with("broken.json"));
    }

    #[test]
    fn file_claiming_builtin_name_is_skipped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let presets = dir.path().join("presets");
        fs::create_dir_all(&presets).expect("create dir");
        fs::write(
            presets.join("fake.json"),
            r#"{"name":"Flat","layout":"ten","gains":[0,0,0,0,0,0,0,0,0,0],"builtin":true}"#,
        )
        .expect("write");
        let store = PresetStore::open(presets).expect("open");
        assert_eq!(user_names(&store), Vec::<String>::new());
        assert_eq!(store.load_errors().len(), 1);
        assert_eq!(store.get("Flat").expect("built-in").gains.len(), 10);
    }

    #[test]
    fn loaded_files_never_carry_builtin_flag_and_gains_are_clamped() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        store
            .save(preset(
                "Wild",
                vec![20.0, -20.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            ))
            .expect("save");
        let file = dir.path().join("presets").join("Wild.json");
        let text = fs::read_to_string(file).expect("read file");
        assert!(!text.contains("builtin"));
        let reloaded = open_in(&dir).get("Wild").expect("reloaded");
        assert!(!reloaded.builtin);
        assert_eq!(reloaded.gains[0], GAIN_MAX_DB);
        assert_eq!(reloaded.gains[1], GAIN_MIN_DB);
        assert_eq!(reloaded.gains[2], 5.0);
    }

    #[test]
    fn wrong_band_count_and_non_finite_gains_are_rejected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        assert!(matches!(
            store.save(preset("Short", vec![0.0; 9])),
            Err(PresetError::GainCount {
                expected: 10,
                actual: 9
            })
        ));
        let mut nan = flat_gains();
        nan[3] = f32::NAN;
        assert!(matches!(
            store.save(preset("Nan", nan)),
            Err(PresetError::NonFiniteGain)
        ));
        assert_eq!(user_names(&store).len(), 0);
    }

    #[test]
    fn invalid_names_are_rejected() {
        let bad = [
            "",
            "   ",
            "a<b",
            "a>b",
            "a:b",
            "a\"b",
            "a/b",
            "a\\b",
            "a|b",
            "a?b",
            "a*b",
            "tab\there",
            "trailing.",
            "CON",
            "con",
            "nul.txt",
            "AUX.log",
            "COM1",
            "lpt9.json",
            "COM9 .txt",
        ];
        for name in bad {
            assert!(
                matches!(validate_name(name), Err(PresetError::InvalidName { .. })),
                "`{name}` should be rejected"
            );
        }
        assert!(validate_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn valid_names_are_accepted_and_trimmed() {
        assert_eq!(validate_name("  My Mix  ").expect("valid"), "My Mix");
        assert_eq!(validate_name("Console").expect("valid"), "Console");
        assert_eq!(validate_name("NUL2").expect("valid"), "NUL2");
        assert_eq!(validate_name("COM10").expect("valid"), "COM10");
        assert_eq!(validate_name(&"x".repeat(64)).expect("valid").len(), 64);
    }

    #[test]
    fn saving_leaves_no_temporary_files_behind() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        store.save(preset("Clean", flat_gains())).expect("save");
        let leftovers = fs::read_dir(dir.path().join("presets"))
            .expect("read dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.path().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn delete_removes_the_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut store = open_in(&dir);
        store.save(preset("Gone", flat_gains())).expect("save");
        store.delete("gone").expect("delete");
        assert!(!dir.path().join("presets").join("Gone.json").exists());
        assert!(matches!(
            store.delete("Gone"),
            Err(PresetError::NotFound(_))
        ));
    }
}
