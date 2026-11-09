//! Compiles the Slint user interface into Rust code at build time.

fn main() {
    // The build script has no way to report a failure except by stopping the build, so the
    // Slint diagnostics are printed in full before exiting.
    if let Err(error) = slint_build::compile("ui/app.slint") {
        eprintln!("failed to compile ui/app.slint: {error}");
        std::process::exit(1);
    }
}
