//! Logging setup: one log file per day in the platform log directory.

use std::fs;
use std::path::Path;

use tracing::level_filters::LevelFilter;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

/// Environment variable that overrides the log level, using the `tracing` filter syntax.
pub const LEVEL_ENV: &str = "EQUALIZERER_LOG";

/// Name of the log file. The daily rotation appends the date, for example
/// `equalizerer.log.2026-11-09`.
const FILE_PREFIX: &str = "equalizerer.log";

/// Installs the global logger, writing to `equalizerer.log` in `log_dir`.
///
/// The returned guard flushes the background writer when dropped, so the caller must keep it
/// alive until the process ends. Returns `None` when the file cannot be created; the app then
/// runs without logs and says so on stderr.
#[must_use]
pub fn init(log_dir: &Path) -> Option<WorkerGuard> {
    if let Err(error) = fs::create_dir_all(log_dir) {
        eprintln!(
            "cannot create the log directory {}: {error}",
            log_dir.display()
        );
        return None;
    }
    let appender = match RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(FILE_PREFIX)
        .build(log_dir)
    {
        Ok(appender) => appender,
        Err(error) => {
            eprintln!("cannot open the log file in {}: {error}", log_dir.display());
            return None;
        }
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .with_env_var(LEVEL_ENV)
        .from_env_lossy();
    // `try_init` rather than `init`: a second global subscriber would be a bug, but it must not
    // abort the app.
    let installed = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init();
    match installed {
        Ok(()) => Some(guard),
        Err(error) => {
            eprintln!("logging was already initialized: {error}");
            None
        }
    }
}
