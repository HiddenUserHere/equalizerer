//! Command-line arguments. Parsed by hand: the app has three modes and no options.

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

/// Usage text printed for bad arguments.
pub const USAGE: &str = "usage: eq-app [--demo | --restore]";

/// Parses the arguments after the program name.
///
/// # Errors
///
/// Returns the usage text when an argument is unknown or the two mode flags are combined.
pub fn parse<I>(args: I) -> Result<Mode, String>
where
    I: IntoIterator<Item = String>,
{
    let mut mode = Mode::App;
    let mut seen_flag = false;
    for arg in args {
        let next = match arg.as_str() {
            "--demo" => Mode::Demo,
            "--restore" => Mode::Restore,
            _ => return Err(format!("unknown argument `{arg}`\n{USAGE}")),
        };
        if seen_flag {
            return Err(format!("choose one mode flag, not two\n{USAGE}"));
        }
        seen_flag = true;
        mode = next;
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Mode, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn no_arguments_starts_the_app() {
        assert_eq!(parse_strs(&[]), Ok(Mode::App));
    }

    #[test]
    fn flags_select_their_modes() {
        assert_eq!(parse_strs(&["--demo"]), Ok(Mode::Demo));
        assert_eq!(parse_strs(&["--restore"]), Ok(Mode::Restore));
    }

    #[test]
    fn unknown_and_combined_flags_are_errors() {
        assert!(parse_strs(&["--bogus"]).is_err());
        assert!(parse_strs(&["--demo", "--restore"]).is_err());
    }
}
