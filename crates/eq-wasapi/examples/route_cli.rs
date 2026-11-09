//! Runs the real controller without a UI: routes system audio through the virtual device, EQ and
//! the chosen target, and prints the controller events until Enter is pressed.
//!
//! Usage:
//! `cargo run -p eq-wasapi --example route_cli -- [--target <substring>] [--latency low|auto|reliable] [--virtual <pattern>]`
//!
//! - `--target` makes the device whose name contains the text the Windows default before the
//!   controller starts. It is optional.
//! - `--latency` picks the latency mode. The default is `auto`.
//! - `--virtual` is the name fragment of the virtual playback device. The default is `CABLE Input`.
//!
//! Settings and presets go to a temporary directory, so the real settings stay untouched. The
//! default output device is restored when the example exits through Enter. A hard kill (Ctrl+C
//! with the default handler) skips that restore, so run `eq-app --restore` in that case.

#[cfg(windows)]
mod app {
    use std::error::Error;
    use std::thread;

    use crossbeam_channel::{bounded, select, unbounded};
    use eq_core::{
        AppCommand, AppEvent, AppPaths, AudioBackend, BackendError, Controller, ControllerConfig,
        DeviceEvent, DeviceInfo, EndpointDirectory, LatencyMode, PatternLocator, PresetStore,
        Settings, SettingsStore,
    };
    use eq_wasapi::{ComApartment, MmDirectory, RestoreDefaultGuard, WasapiBackend};

    /// Usage text printed for `--help` and for bad arguments.
    const USAGE: &str = "usage: route_cli [--target <substring>] [--latency low|auto|reliable] \
[--virtual <pattern>]";

    /// Parsed command-line options.
    struct Options {
        /// Name fragment of the device to make the Windows default before starting.
        target: Option<String>,
        /// Latency mode for the pipeline.
        latency: LatencyMode,
        /// Name fragment of the virtual playback device.
        virtual_pattern: String,
    }

    /// Parses the arguments after the program name. Unknown flags and missing values are errors.
    fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
        let mut options = Options {
            target: None,
            latency: LatencyMode::Auto,
            virtual_pattern: "CABLE Input".to_owned(),
        };
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--target" => options.target = Some(value(&mut args, &flag)?),
                "--latency" => options.latency = parse_latency(&value(&mut args, &flag)?)?,
                "--virtual" => options.virtual_pattern = value(&mut args, &flag)?,
                "--help" | "-h" => return Err(USAGE.to_owned()),
                other => return Err(format!("unknown argument `{other}`\n{USAGE}")),
            }
        }
        Ok(options)
    }

    /// Takes the value that follows `flag`.
    fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
        args.next()
            .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
    }

    /// Maps the command-line spelling of a latency mode.
    fn parse_latency(text: &str) -> Result<LatencyMode, String> {
        match text.to_ascii_lowercase().as_str() {
            "low" => Ok(LatencyMode::Low),
            "auto" => Ok(LatencyMode::Auto),
            "reliable" => Ok(LatencyMode::Reliable),
            other => Err(format!(
                "unknown latency `{other}` (use low, auto or reliable)"
            )),
        }
    }

    /// Picks the first non-virtual output whose name contains `needle`, ignoring case.
    fn find_target(devices: &[DeviceInfo], needle: &str) -> Result<DeviceInfo, String> {
        let needle = needle.to_lowercase();
        devices
            .iter()
            .find(|device| !device.is_virtual && device.name.to_lowercase().contains(&needle))
            .cloned()
            .ok_or_else(|| format!("no output device name contains `{needle}`"))
    }

    /// Prints one controller event on one line.
    fn print_event(event: &AppEvent) {
        match event {
            AppEvent::Snapshot(snapshot) => println!(
                "power={} status={:?} target={} latency={}",
                snapshot.power,
                snapshot.status,
                snapshot.target_name.as_deref().unwrap_or("-"),
                snapshot.latency.label(),
            ),
            AppEvent::Stats(stats) => println!(
                "latency={:.1} ms underruns={} resyncs={}",
                stats.latency_ms, stats.underruns, stats.resyncs,
            ),
            AppEvent::Error(message) => eprintln!("error: {message}"),
        }
    }

    /// Runs the example until Enter is pressed or the controller stops.
    pub fn run() -> Result<(), Box<dyn Error>> {
        let options = parse_args(std::env::args().skip(1))?;

        let paths = AppPaths::from_root(std::env::temp_dir().join("equalizerer-route-cli"));
        std::fs::create_dir_all(&paths.config_dir)?;
        let presets = PresetStore::open(paths.presets_dir.clone())?;
        let settings_store = SettingsStore::new(paths.settings_file.clone());
        let settings = Settings {
            power_on: true,
            latency_mode: options.latency,
            virtual_device_pattern: options.virtual_pattern.clone(),
            ..Settings::default()
        };

        // Declared after `directory`, so it drops first: it restores the default device before
        // the enumerator and the COM apartment are released. It must outlive the controller,
        // which is joined before this function returns.
        let com = ComApartment::init_mta()?;
        let directory = MmDirectory::new(&com, PatternLocator::new(&options.virtual_pattern))?;
        let restore = RestoreDefaultGuard::new(&directory)?;
        if let Some(needle) = &options.target {
            let devices = directory.list_render()?;
            let target = find_target(&devices, needle)?;
            println!("target: {} ({})", target.name, target.id);
            directory.set_default_render(&target.id)?;
        }

        let (cmd_tx, cmd_rx) = unbounded::<AppCommand>();
        let (dev_tx, dev_rx) = unbounded::<DeviceEvent>();
        let (app_tx, app_rx) = unbounded::<AppEvent>();
        let pattern = options.virtual_pattern.clone();
        let controller = Controller::spawn(
            move || {
                WasapiBackend::new(dev_tx, &pattern)
                    .map(|backend| Box::new(backend) as Box<dyn AudioBackend>)
                    .map_err(BackendError::from)
            },
            presets,
            settings_store,
            settings,
            ControllerConfig::default(),
            app_tx,
            cmd_rx,
            dev_rx,
        )?;

        // Enter is read on its own thread, so the main loop can wait on events and on Enter.
        let (enter_tx, enter_rx) = bounded::<()>(1);
        thread::spawn(move || {
            let mut line = String::new();
            // An error (no console input) also ends the run, as if Enter had been pressed.
            let _ = std::io::stdin().read_line(&mut line);
            let _ = enter_tx.send(());
        });

        println!("Routing. Press Enter to stop and restore the default device.");
        loop {
            select! {
                recv(app_rx) -> event => match event {
                    Ok(event) => print_event(&event),
                    // The controller has exited; nothing more to show.
                    Err(_) => break,
                },
                recv(enter_rx) -> _ => break,
            }
        }

        // Quit powers the controller off, which makes the target the default again. Events
        // still in flight are printed until the controller drops its sender.
        let _ = cmd_tx.send(AppCommand::Quit);
        for event in app_rx {
            print_event(&event);
        }
        controller
            .join()
            .map_err(|_| "controller thread panicked")?;
        drop(restore);
        Ok(())
    }
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("route_cli is only available on Windows");
}
