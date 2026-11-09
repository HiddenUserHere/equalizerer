//! Prints device events (default changes, additions, removals) until Enter is pressed.
//!
//! Read-only: it only listens for notifications.
//!
//! Usage: `cargo run -p eq-wasapi --example watch_devices`

#[cfg(windows)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use eq_core::PatternLocator;
    use eq_wasapi::{ComApartment, MmDirectory, NotificationGuard};

    let apartment = ComApartment::init_mta()?;
    let directory = MmDirectory::new(&apartment, PatternLocator::default())?;
    let (tx, rx) = crossbeam_channel::unbounded();
    // Dropped at the end of `main`, which unregisters the client.
    let _guard = NotificationGuard::register(directory.enumerator(), tx)?;

    // A thread waits for Enter and reports it on a channel, so the main loop can select on
    // both device events and the stop request.
    let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        // Errors (for example, no console input) also stop the watch.
        let _ = std::io::stdin().read_line(&mut line);
        let _ = stop_tx.send(());
    });

    println!("Watching device events. Press Enter to stop.");
    loop {
        crossbeam_channel::select! {
            recv(rx) -> event => match event {
                Ok(event) => println!("{event:?}"),
                Err(_) => break,
            },
            recv(stop_rx) -> _ => break,
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("watch_devices is only available on Windows");
}
