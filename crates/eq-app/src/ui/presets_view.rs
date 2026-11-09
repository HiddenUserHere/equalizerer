//! View model of the preset controls: the grouped popup list, which actions are enabled, the
//! name rules of the dialogs and the command each dialog sends.
//!
//! Pure functions only, so grouping and validation are covered by unit tests.

use eq_core::{AppCommand, PresetSummary};

/// Heading of the built-in group in the preset list.
pub const BUILTIN_HEADING: &str = "Built-in";

/// Heading of the user group in the preset list.
pub const USER_HEADING: &str = "My presets";

/// Label shown when no preset is active, because the gains were edited or never loaded.
pub const CUSTOM_LABEL: &str = "Custom";

/// One line of the preset popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PresetRow {
    /// A group heading. It is not selectable.
    Heading(&'static str),
    /// A preset entry.
    Item {
        /// Preset name.
        name: String,
        /// `true` for the active preset, which is highlighted.
        active: bool,
    },
}

/// Builds the popup list: the built-in group first, then the user group. A group without entries
/// is left out together with its heading. Entries keep the order of `presets`.
#[must_use]
pub fn build_rows(presets: &[PresetSummary], active: Option<&str>) -> Vec<PresetRow> {
    let mut rows = Vec::with_capacity(presets.len() + 2);
    for (builtin, heading) in [(true, BUILTIN_HEADING), (false, USER_HEADING)] {
        let mut group = presets
            .iter()
            .filter(|preset| preset.builtin == builtin)
            .peekable();
        if group.peek().is_none() {
            continue;
        }
        rows.push(PresetRow::Heading(heading));
        rows.extend(group.map(|preset| PresetRow::Item {
            name: preset.name.clone(),
            active: Some(preset.name.as_str()) == active,
        }));
    }
    rows
}

/// Text of the dropdown: the active preset's name, or [`CUSTOM_LABEL`].
#[must_use]
pub fn selector_label(active: Option<&str>) -> &str {
    active.unwrap_or(CUSTOM_LABEL)
}

/// Which preset actions can be used right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetActions {
    /// "Save": overwrite the active user preset with modified gains.
    pub save: bool,
    /// "Rename…": the active preset is a user preset.
    pub rename: bool,
    /// "Delete": the active preset is a user preset.
    pub delete: bool,
}

/// Decides the enabled preset actions. Built-in presets can never be saved, renamed or deleted,
/// and "Save as…" is always available, so it is not part of the result.
#[must_use]
pub fn actions(presets: &[PresetSummary], active: Option<&str>, modified: bool) -> PresetActions {
    let user = active
        .and_then(|name| presets.iter().find(|preset| preset.name == name))
        .is_some_and(|preset| !preset.builtin);
    PresetActions {
        save: user && modified,
        rename: user,
        delete: user,
    }
}

/// Why a name cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    /// Nothing but whitespace was entered.
    Empty,
    /// Another preset already has this name, ignoring case. Holds the trimmed name.
    Duplicate(String),
    /// Renaming a preset to the name it already has. This is a no-op, not an error to show.
    Unchanged,
}

impl NameError {
    /// Inline message for the dialog. `None` means the dialog only keeps OK disabled.
    #[must_use]
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Empty => Some("Enter a name.".to_owned()),
            Self::Duplicate(name) => Some(format!(
                "A preset named \u{201c}{name}\u{201d} already exists."
            )),
            Self::Unchanged => None,
        }
    }
}

/// Checks a preset name typed into a dialog and returns it trimmed.
///
/// `original` is the current name when renaming. A rename that only changes the letter case
/// is allowed, so the original name is not counted as a duplicate.
///
/// # Errors
///
/// Returns [`NameError::Empty`] for blank input, [`NameError::Unchanged`] when renaming to the
/// same name, and [`NameError::Duplicate`] when another preset has the name (ignoring case).
pub fn validate_name(
    candidate: &str,
    existing: &[PresetSummary],
    original: Option<&str>,
) -> Result<String, NameError> {
    let name = candidate.trim();
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if original == Some(name) {
        return Err(NameError::Unchanged);
    }
    let lowered = name.to_lowercase();
    let taken = existing.iter().any(|preset| {
        Some(preset.name.as_str()) != original && preset.name.to_lowercase() == lowered
    });
    if taken {
        Err(NameError::Duplicate(name.to_owned()))
    } else {
        Ok(name.to_owned())
    }
}

/// A modal dialog of the presets UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogKind {
    /// Creates a user preset from the current gains.
    SaveAs,
    /// Renames the user preset `from`.
    Rename {
        /// Current name of the preset.
        from: String,
    },
    /// Asks whether to delete the user preset `name`.
    Delete {
        /// Name of the preset to delete.
        name: String,
    },
}

impl DialogKind {
    /// Title shown at the top of the dialog.
    #[must_use]
    pub fn title(&self) -> String {
        match self {
            Self::SaveAs => "Save preset as".to_owned(),
            Self::Rename { .. } => "Rename preset".to_owned(),
            Self::Delete { name } => format!("Delete preset \u{201c}{name}\u{201d}?"),
        }
    }

    /// Label of the confirming button.
    #[must_use]
    pub const fn ok_label(&self) -> &'static str {
        match self {
            Self::SaveAs => "Save",
            Self::Rename { .. } => "Rename",
            Self::Delete { .. } => "Delete",
        }
    }

    /// `true` when the dialog has a name field (every dialog except the delete confirmation).
    #[must_use]
    pub const fn needs_name(&self) -> bool {
        !matches!(self, Self::Delete { .. })
    }

    /// Text the name field starts with: the current name when renaming, otherwise empty.
    #[must_use]
    pub fn initial_text(&self) -> String {
        match self {
            Self::Rename { from } => from.clone(),
            Self::SaveAs | Self::Delete { .. } => String::new(),
        }
    }

    /// The command sent when the dialog is confirmed. The name is trimmed here, so the
    /// controller receives the same name that was validated.
    #[must_use]
    pub fn command(&self, text: &str) -> AppCommand {
        match self {
            Self::SaveAs => AppCommand::SavePresetAs(text.trim().to_owned()),
            Self::Rename { from } => AppCommand::RenamePreset {
                from: from.clone(),
                to: text.trim().to_owned(),
            },
            Self::Delete { name } => AppCommand::DeletePreset(name.clone()),
        }
    }
}

/// What a dialog shows for the text in its name field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogCheck {
    /// Inline message under the field, if any.
    pub message: Option<String>,
    /// `true` when the confirming button may be used.
    pub ok_enabled: bool,
}

/// Validates the text of a dialog. Empty input shows no message yet (the disabled OK button
/// says enough); whitespace-only input shows "Enter a name.".
#[must_use]
pub fn check_dialog(kind: &DialogKind, text: &str, presets: &[PresetSummary]) -> DialogCheck {
    if !kind.needs_name() {
        return DialogCheck {
            message: None,
            ok_enabled: true,
        };
    }
    let original = match kind {
        DialogKind::Rename { from } => Some(from.as_str()),
        DialogKind::SaveAs | DialogKind::Delete { .. } => None,
    };
    match validate_name(text, presets, original) {
        Ok(_) => DialogCheck {
            message: None,
            ok_enabled: true,
        },
        Err(error) => DialogCheck {
            message: if text.is_empty() {
                None
            } else {
                error.message()
            },
            ok_enabled: false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preset(name: &str, builtin: bool) -> PresetSummary {
        PresetSummary {
            name: name.to_owned(),
            builtin,
        }
    }

    fn library() -> Vec<PresetSummary> {
        vec![
            preset("Flat", true),
            preset("Bass Boost", true),
            preset("Rock", true),
            preset("Late Night", false),
            preset("Podcast", false),
        ]
    }

    #[test]
    fn rows_are_grouped_with_headings_and_active_is_marked() {
        let rows = build_rows(&library(), Some("Podcast"));
        assert_eq!(
            rows,
            vec![
                PresetRow::Heading("Built-in"),
                PresetRow::Item {
                    name: "Flat".to_owned(),
                    active: false
                },
                PresetRow::Item {
                    name: "Bass Boost".to_owned(),
                    active: false
                },
                PresetRow::Item {
                    name: "Rock".to_owned(),
                    active: false
                },
                PresetRow::Heading("My presets"),
                PresetRow::Item {
                    name: "Late Night".to_owned(),
                    active: false
                },
                PresetRow::Item {
                    name: "Podcast".to_owned(),
                    active: true
                },
            ]
        );
    }

    #[test]
    fn user_heading_is_omitted_without_user_presets() {
        let presets = [preset("Flat", true), preset("Rock", true)];
        let rows = build_rows(&presets, None);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, PresetRow::Heading(_)))
                .count(),
            1
        );
        assert!(!rows.contains(&PresetRow::Heading(USER_HEADING)));
    }

    #[test]
    fn no_rows_for_an_empty_library() {
        assert_eq!(build_rows(&[], None), Vec::<PresetRow>::new());
    }

    #[test]
    fn no_row_is_active_without_an_active_preset() {
        let rows = build_rows(&library(), None);
        assert!(
            rows.iter()
                .all(|row| !matches!(row, PresetRow::Item { active: true, .. }))
        );
    }

    #[test]
    fn selector_shows_custom_without_an_active_preset() {
        assert_eq!(selector_label(None), "Custom");
        assert_eq!(selector_label(Some("Rock")), "Rock");
    }

    #[test]
    fn builtin_presets_never_allow_save_rename_or_delete() {
        let a = actions(&library(), Some("Rock"), true);
        assert_eq!(
            a,
            PresetActions {
                save: false,
                rename: false,
                delete: false
            }
        );
    }

    #[test]
    fn user_preset_allows_rename_and_delete_and_save_only_when_modified() {
        let clean = actions(&library(), Some("Podcast"), false);
        assert!(clean.rename && clean.delete && !clean.save);
        let dirty = actions(&library(), Some("Podcast"), true);
        assert!(dirty.save && dirty.rename && dirty.delete);
    }

    #[test]
    fn no_active_preset_allows_nothing() {
        let a = actions(&library(), None, true);
        assert!(!a.save && !a.rename && !a.delete);
    }

    #[test]
    fn names_are_trimmed() {
        assert_eq!(
            validate_name("  Podcast Two  ", &library(), None),
            Ok("Podcast Two".to_owned())
        );
    }

    #[test]
    fn blank_names_are_rejected() {
        assert_eq!(validate_name("", &library(), None), Err(NameError::Empty));
        assert_eq!(
            validate_name(" \t ", &library(), None),
            Err(NameError::Empty)
        );
    }

    #[test]
    fn duplicates_are_rejected_ignoring_case() {
        assert_eq!(
            validate_name("ROCK", &library(), None),
            Err(NameError::Duplicate("ROCK".to_owned()))
        );
        assert_eq!(
            validate_name(" late night ", &library(), None),
            Err(NameError::Duplicate("late night".to_owned()))
        );
    }

    #[test]
    fn duplicate_message_names_the_preset() {
        let error = NameError::Duplicate("Rock".to_owned());
        assert_eq!(
            error.message().as_deref(),
            Some("A preset named \u{201c}Rock\u{201d} already exists.")
        );
        assert_eq!(NameError::Empty.message().as_deref(), Some("Enter a name."));
        assert_eq!(NameError::Unchanged.message(), None);
    }

    #[test]
    fn renaming_to_the_same_name_is_unchanged() {
        assert_eq!(
            validate_name("Podcast", &library(), Some("Podcast")),
            Err(NameError::Unchanged)
        );
    }

    #[test]
    fn renaming_only_the_letter_case_is_allowed() {
        assert_eq!(
            validate_name("PODCAST", &library(), Some("Podcast")),
            Ok("PODCAST".to_owned())
        );
    }

    #[test]
    fn renaming_onto_another_preset_is_a_duplicate() {
        assert_eq!(
            validate_name("rock", &library(), Some("Podcast")),
            Err(NameError::Duplicate("rock".to_owned()))
        );
    }

    #[test]
    fn save_as_dialog_has_no_message_while_empty_and_disables_ok() {
        let check = check_dialog(&DialogKind::SaveAs, "", &library());
        assert_eq!(
            check,
            DialogCheck {
                message: None,
                ok_enabled: false
            }
        );
    }

    #[test]
    fn whitespace_in_a_dialog_shows_the_empty_message() {
        let check = check_dialog(&DialogKind::SaveAs, "   ", &library());
        assert_eq!(check.message.as_deref(), Some("Enter a name."));
        assert!(!check.ok_enabled);
    }

    #[test]
    fn duplicate_in_a_dialog_shows_a_message_and_disables_ok() {
        let check = check_dialog(&DialogKind::SaveAs, "rock", &library());
        assert!(check.message.is_some());
        assert!(!check.ok_enabled);
    }

    #[test]
    fn rename_dialog_starts_disabled_because_nothing_changed() {
        let kind = DialogKind::Rename {
            from: "Podcast".to_owned(),
        };
        assert_eq!(kind.initial_text(), "Podcast");
        let check = check_dialog(&kind, "Podcast", &library());
        assert_eq!(
            check,
            DialogCheck {
                message: None,
                ok_enabled: false
            }
        );
    }

    #[test]
    fn delete_dialog_has_no_name_field_and_ok_is_always_enabled() {
        let kind = DialogKind::Delete {
            name: "Podcast".to_owned(),
        };
        assert!(!kind.needs_name());
        assert_eq!(kind.title(), "Delete preset \u{201c}Podcast\u{201d}?");
        assert_eq!(kind.ok_label(), "Delete");
        assert_eq!(
            check_dialog(&kind, "", &library()),
            DialogCheck {
                message: None,
                ok_enabled: true
            }
        );
    }

    #[test]
    fn dialogs_send_the_matching_commands() {
        assert_eq!(
            DialogKind::SaveAs.command("  New  "),
            AppCommand::SavePresetAs("New".to_owned())
        );
        assert_eq!(
            DialogKind::Rename {
                from: "Podcast".to_owned()
            }
            .command(" Show "),
            AppCommand::RenamePreset {
                from: "Podcast".to_owned(),
                to: "Show".to_owned()
            }
        );
        assert_eq!(
            DialogKind::Delete {
                name: "Podcast".to_owned()
            }
            .command(""),
            AppCommand::DeletePreset("Podcast".to_owned())
        );
    }
}
