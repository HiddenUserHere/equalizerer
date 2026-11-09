//! Prints the active output devices, marking the default and the virtual device.
//!
//! Read-only: it does not change any setting.
//!
//! Usage: `cargo run -p eq-wasapi --example list_devices`

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eq_core::{EndpointDirectory, PatternLocator};
    use eq_wasapi::{ComApartment, MmDirectory};

    let apartment = ComApartment::init_mta()?;
    let directory = MmDirectory::new(&apartment, PatternLocator::default())?;
    let default = directory.default_render()?;

    for device in directory.list_render()? {
        let mut marks = String::new();
        if default.as_ref() == Some(&device.id) {
            marks.push_str(" [default]");
        }
        if device.is_virtual {
            marks.push_str(" [virtual]");
        }
        println!("{}  {}{}", device.id, device.name, marks);
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("list_devices is only available on Windows");
}
