# Equalizerer

A cross-platform (Windows and Linux) system-wide equalizer written in Rust. The app routes
everything applications play through a virtual output device, applies a graphic EQ and renders
the result to the real device you chose.

MVP scope: power button, 10-band graphic EQ (32 Hz to 16 kHz) with presets, and a latency
system (Lower Latency / Automatic / More Reliable).

- Windows: WASAPI, with VB-CABLE as the virtual device.
- Linux: PipeWire, with a virtual sink the app creates at runtime.

## Documentation

- [AGENTS.md](AGENTS.md): rules for contributors and AI coding agents.
- [docs/README.md](docs/README.md): index of all design and planning documents.

## Build and check

```sh
cargo build --workspace                      # build
cargo test --workspace                       # tests
./scripts/check.ps1                          # Windows: fmt, clippy, tests
./scripts/check.sh                           # Linux / WSL2: fmt, clippy, tests
cargo run -p eq-app --release                # run the app
```

Linux build dependencies (Debian/Ubuntu package names): `build-essential pkg-config clang
libpipewire-0.3-dev libspa-0.2-dev libfontconfig-dev libxkbcommon-dev`.

## License

MIT
