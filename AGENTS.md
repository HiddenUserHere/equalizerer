# AGENTS.md

Instructions for AI coding agents (and humans) working on **Equalizerer** — a cross-platform
(**Windows and Linux**) system-wide equalizer written in Rust.

## What this project is

The app makes a virtual output device the system default, captures everything apps play into
it, applies a graphic EQ, and renders the result to the real device the user chose. When the user
picks another device in the OS, the app adopts it as the new output and re-selects the virtual
device.

- **Windows:** WASAPI; the virtual device is VB-CABLE (installed by the user).
- **Linux:** PipeWire; the app creates its own virtual sink ("Equalizerer") at runtime.

## Current scope (MVP)

Only these features are in scope right now, and each must work on both Windows and Linux:

1. **Power button** — turn processing/routing on and off (off restores the real device as default).
2. **10-band graphic EQ** (32 Hz – 16 kHz, ±12 dB).
3. **Multiple presets** — built-in presets plus user presets (save, rename, delete, select).
4. **Latency system** — Lower Latency / Automatic / More Reliable, with measured latency shown.

Everything else (31 bands, tray, device list UI, AutoEq import, volume mirroring, own driver, …)
is in the backlog in [docs/PLAN.md](docs/PLAN.md). Do not implement backlog items unless the task
says so, but keep designs open to them (e.g. `Layout` is an enum even though it has one variant).

## Documentation map

| File | Contents |
|---|---|
| [docs/README.md](docs/README.md) | Index of all docs |
| [docs/architecture.md](docs/architecture.md) | Architecture, threading, DSP and routing design |
| [docs/PLAN.md](docs/PLAN.md) | MVP task list (with acceptance criteria) and backlog |

Read `docs/architecture.md` before writing code. Update docs in the same change when behaviour
or structure changes.

## Language

- **All code, identifiers, comments, doc comments, docs, commit messages and PR text are in English.**
- When editing a file that contains non-English text, translate it as part of the edit.
- Conversation with the user follows the user's language.

## Workflow

- **Every implementation task is delegated to a subagent running model `haiku`** — one agent per
  task from `docs/PLAN.md`, given the full task block plus a pointer to `docs/architecture.md`.
- The main session reviews the diff and re-runs the checks before marking a task done.
- A task is done only when its acceptance criteria are met and the check scripts pass:
  `scripts/check.ps1` on Windows, and `scripts/check.sh` (Linux or WSL2) whenever shared crates or
  `eq-pipewire` changed. Shared crates must build on both OSes.
- Keep changes scoped to the task. No drive-by refactors.

## Commands

```sh
cargo build --workspace                 # build
cargo test --workspace                  # tests
./scripts/check.ps1                     # Windows: fmt --check + clippy -D warnings + tests (must pass)
./scripts/check.sh                      # Linux/WSL2: same checks
cargo bench -p eq-dsp                   # DSP benchmarks
cargo run -p eq-app --release           # run the app
cargo run -p eq-app --release -- --restore   # restore the real default device and exit
```

Linux build dependencies (Debian/Ubuntu names): `build-essential pkg-config clang
libpipewire-0.3-dev libspa-0.2-dev libfontconfig-dev libxkbcommon-dev`.

## Workspace layout and dependency rules

```
crates/eq-dsp       pure DSP, #![no_std], no dependencies, allocation-free hot path
crates/eq-engine    platform-agnostic real-time blocks: FIFO, drift resampler, processor, param/stat handoff
crates/eq-core      domain: model, ports (traits), router state machine, presets, settings, controller
crates/eq-wasapi    Windows backend (cfg(windows)): COM, MMDevice, IPolicyConfig, notifications, pipeline
crates/eq-pipewire  Linux backend (cfg(target_os = "linux")): PipeWire registry, metadata, null sink, streams
crates/eq-app       binary: composition root, backend selection, Slint UI
```

- Direction: `eq-app → {eq-wasapi | eq-pipewire} → {eq-core, eq-engine} → eq-dsp`. Never the other way.
- `eq-dsp`, `eq-engine` and `eq-core` must not depend on OS-specific crates and must build on both OSes.
- Platform code lives only in the two backend crates. `eq-app` is the only place that picks a
  backend (`#[cfg(target_os = …)]`); everything else talks to the `AudioBackend` ports.
- Never put `#[cfg(windows)]` / `#[cfg(target_os = "linux")]` branches inside shared crates,
  except `eq-engine`'s denormal guard (per CPU architecture) and file-name rules that must be
  identical on all OSes anyway.
- The router is a pure state machine: input in, effects out. I/O happens only in the controller
  and adapters.

## Code rules

- Rust stable, edition 2024, `x86_64-pc-windows-msvc`.
- No `unwrap()` / `expect()` outside tests and early `main` setup.
- Library errors: `thiserror` enums. `anyhow` only in `eq-app`.
- Every public item has a doc comment. Comments explain *why*, not *what*.
- `unsafe` only in `eq-wasapi`, `eq-pipewire` (FFI) and `eq-engine`'s denormal guard; every
  block carries a `// SAFETY:` comment.
- Paths via the `directories` crate; never hard-code `%APPDATA%` or `~/.config`.
- Shell scripts use LF line endings (`.gitattributes`); PowerShell scripts may use CRLF.
- Clippy pedantic is on; fix warnings, don't `allow` them without a comment saying why.
- Prefer small modules with one responsibility; names follow the domain terms in
  `docs/architecture.md` (target device, virtual device, latency mode, preset, band).

## Real-time audio rules (the audio thread)

Code that runs in the real-time audio context — the WASAPI RT thread in `eq-wasapi`, the
PipeWire `process` callbacks in `eq-pipewire`, and everything they call (`eq-engine::Processor`,
`GraphicEq::process_interleaved`, …) — must:

- never allocate or free memory, lock a mutex, log, or block (other than the WASAPI wait);
- read parameters only through the triple buffer, once per period;
- publish stats only through atomics;
- keep FTZ/DAZ enabled (no denormal slowdowns).

Allocation-freedom is asserted in tests with `assert_no_alloc`. Breaking these rules causes
audible glitches; treat it as a bug.

## Testing

- DSP: numeric tests against analytic filter responses (`approx`), property tests where useful.
- Core: router and controller scenarios with fake ports — no Windows needed.
- Backends: runnable `examples/` plus the manual checklist in `docs/testing.md`, run on both OSes.
- Linux audio behaviour must be verified on a real Linux desktop with PipeWire; WSL2 is only for
  building and unit tests. Tests that need PipeWire skip themselves when it is not running.
- Do not commit audio files or copyrighted preset data; generate test fixtures in code.

## Safety notes for agents

- Changing the default audio device affects the user's whole system. Examples and tests that
  switch devices must restore the previous default on exit (use the restore guard), and on
  Linux must destroy any virtual sink they create.
- Do not install drivers or change system security settings (e.g. test-signing); ask the user.
