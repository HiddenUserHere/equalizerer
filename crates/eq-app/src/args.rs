//! Command-line arguments. Parsed by hand: the app has a few modes and one option.

/// What the process was started to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The normal application: controller, real audio backend and window.
    App,
    /// The window with the temporary demo controller; no audio side effects.
    Demo,
    /// Restore the persisted target device as the default output, then exit without a window.
    Restore,
}

/// The parsed command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// What the process does.
    pub mode: Mode,
    /// Start with the window hidden, so only the tray icon is visible. The "Start with Windows"
    /// entry passes this flag.
    pub start_minimized: bool,
}

/// Usage text printed for bad arguments.
pub const USAGE: &str = "usage: eq-app [--demo | --restore] [--minimized]";

/// Parses the arguments after the program name.
///
/// # Errors
///
/// Returns the usage text when an argument is unknown, two mode flags are combined, or
/// `--minimized` is combined with `--restore`, which has no window to hide.
pub fn parse<I>(args: I) -> Result<Options, String>
where
    I: IntoIterator<Item = String>,
{
    let mut mode: Option<Mode> = None;
    let mut start_minimized = false;
    for arg in args {
        match arg.as_str() {
            "--minimized" => start_minimized = true,
            "--demo" | "--restore" => {
                if mode.is_some() {
                    return Err(format!("choose one mode flag, not two\n{USAGE}"));
                }
                mode = Some(if arg == "--demo" {
                    Mode::Demo
                } else {
                    Mode::Restore
                });
            }
            _ => return Err(format!("unknown argument `{arg}`\n{USAGE}")),
        }
    }
    let mode = mode.unwrap_or(Mode::App);
    if start_minimized && mode == Mode::Restore {
        return Err(format!("--minimized has no effect with --restore\n{USAGE}"));
    }
    Ok(Options {
        mode,
        start_minimized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Options, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    fn options(mode: Mode, start_minimized: bool) -> Options {
        Options {
            mode,
            start_minimized,
        }
    }

    #[test]
    fn no_arguments_starts_the_app() {
        assert_eq!(parse_strs(&[]), Ok(options(Mode::App, false)));
    }

    #[test]
    fn flags_select_their_modes() {
        assert_eq!(parse_strs(&["--demo"]), Ok(options(Mode::Demo, false)));
        assert_eq!(
            parse_strs(&["--restore"]),
            Ok(options(Mode::Restore, false))
        );
    }

    #[test]
    fn minimized_starts_the_app_hidden() {
        assert_eq!(parse_strs(&["--minimized"]), Ok(options(Mode::App, true)));
        assert_eq!(
            parse_strs(&["--demo", "--minimized"]),
            Ok(options(Mode::Demo, true))
        );
        assert_eq!(
            parse_strs(&["--minimized", "--demo"]),
            Ok(options(Mode::Demo, true))
        );
    }

    #[test]
    fn minimized_is_refused_with_restore() {
        assert!(parse_strs(&["--restore", "--minimized"]).is_err());
    }

    #[test]
    fn unknown_and_combined_flags_are_errors() {
        assert!(parse_strs(&["--bogus"]).is_err());
        assert!(parse_strs(&["--demo", "--restore"]).is_err());
    }
}
