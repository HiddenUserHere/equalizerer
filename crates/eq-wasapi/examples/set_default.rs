//! Makes the first output whose name contains the argument the default, waits for Enter, then
//! restores the previous default.
//!
//! Changes the system default output while it runs. Ctrl+C skips the restore, so press Enter.
//!
//! Usage: `cargo run -p eq-wasapi --example set_default -- <name-substring>`

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eq_core::{EndpointDirectory, PatternLocator};
    use eq_wasapi::{ComApartment, MmDirectory, RestoreDefaultGuard};

    let pattern = std::env::args()
        .nth(1)
        .ok_or("usage: set_default <name-substring>")?;
    let needle = pattern.to_lowercase();

    let apartment = ComApartment::init_mta()?;
    let directory = MmDirectory::new(&apartment, PatternLocator::default())?;
    let target = directory
        .list_render()?
        .into_iter()
        .find(|device| device.name.to_lowercase().contains(&needle))
        .ok_or_else(|| format!("no output device name contains `{pattern}`"))?;

    // Declared after `directory`, so it drops first and restores while the enumerator is alive.
    let _restore = RestoreDefaultGuard::new(&directory)?;
    directory.set_default_render(&target.id)?;
    println!(
        "Default output set to {} ({}). Press Enter to restore the previous default.",
        target.name, target.id
    );

    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    println!("Restoring the previous default.");
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("set_default is only available on Windows");
}
