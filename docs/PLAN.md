# Plan — MVP tasks and backlog

MVP scope: **power button, 10-band EQ with multiple presets, latency system — on Windows and Linux.**
Design reference: [architecture.md](architecture.md). Rules: [../AGENTS.md](../AGENTS.md).

## How tasks run

- Each task is executed by **one subagent on model `haiku`**, given this task block and
  `docs/architecture.md`.
- Done = acceptance criteria met **and** the check scripts pass (`scripts/check.ps1` on Windows;
  `scripts/check.sh` on Linux/WSL for any task touching shared crates or `eq-pipewire`).
  The main session reviews the diff before marking it done.
- Platform-specific acceptance steps say which OS they need. Linux audio steps need a real
  Linux desktop with PipeWire; WSL2 covers build and unit tests only.
- Status legend: `[ ]` todo, `[~]` in progress, `[x]` done.

### Order and parallelism

```
P0 ─┬─► P1 (eq-dsp) ──► P2 (eq-engine) ──┐
    ├─► P3 (eq-core) ────────────────────┼─► P4.5–4.6 (Windows pipeline) ─┐
    ├─► P4.1–4.4 (Windows devices) ──────┤                                ├─► P6 (app/UI) ─► P7 (QA)
    └─► P5.1–5.3 (Linux devices) ────────┴─► P5.4–5.5 (Linux pipeline) ───┘
```

P1, P3, P4.1–4.4 and P5.1–5.3 touch different crates and can run in parallel.

---

## Phase 0 — Setup

**[~] T-0.1 Windows toolchain** (Rust 1.99.1 installed; VB-CABLE still missing) *(user action)*
- Install `rustup` (stable, `x86_64-pc-windows-msvc`), Visual Studio Build Tools with
  "Desktop development with C++", and VB-CABLE.
- Acceptance: `cargo --version` works in a new terminal; "CABLE Input" appears in Windows sound output.

**[ ] T-0.2 Linux environment** *(user action)*
- Build/test: WSL2 Ubuntu (24.04+) with `rustup`, `build-essential`, `pkg-config`, `clang`,
  `libpipewire-0.3-dev`, `libspa-0.2-dev`, `libfontconfig-dev`, `libxkbcommon-dev`.
- Audio testing: a real Linux desktop (or VM with audio) running PipeWire + WirePlumber.
- Acceptance: `cargo --version` in WSL; `pw-cli info 0` works on the audio test machine.

**[x] T-0.3 Workspace scaffold**
- Files: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `.gitattributes` (LF for `*.sh`),
  `rustfmt.toml`, `README.md`, `crates/{eq-dsp,eq-engine,eq-core,eq-wasapi,eq-pipewire}/{Cargo.toml,src/lib.rs}`,
  `crates/eq-app/{Cargo.toml,src/main.rs}`.
- Steps: `git init`; edition 2024; dependency direction from `AGENTS.md`;
  `eq-wasapi` and `eq-pipewire` compile to an empty crate on the other OS
  (`#![cfg(windows)]` / `#![cfg(target_os = "linux")]` at crate root);
  `eq-app` uses `[target.'cfg(windows)'.dependencies]` / `[target.'cfg(target_os = "linux")'.dependencies]`;
  `[workspace.lints]` (clippy pedantic = warn, `unsafe_code` = deny, allowed in the two backends);
  release profile `lto = "fat"`, `codegen-units = 1`, `opt-level = 3`, `strip = true`;
  `eq-dsp` is `#![no_std]`.
- Acceptance: `cargo build --workspace` and `cargo test --workspace` pass on Windows and in WSL.

**[x] T-0.4 Check scripts**
- `scripts/check.ps1` and `scripts/check.sh`: `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; stop and exit
  non-zero on the first failure.
- Acceptance: both pass on the scaffold; both fail when a formatting error is introduced.

**[ ] T-0.5 CI (when a GitHub remote exists)**
- `.github/workflows/ci.yml`: matrix `windows-latest` + `ubuntu-latest` (install the PipeWire
  dev packages), running the check script; cache with `Swatinem/rust-cache`.
- Acceptance: green on both runners.

## Phase 1 — DSP (`crates/eq-dsp`)

**[x] T-1.1 Biquad core** — `src/biquad.rs`
- `Coeffs { b0, b1, b2, a1, a2: f64 }` (a0 normalised to 1), `Coeffs::IDENTITY`.
- `StereoBiquad { coeffs, s1: [f64; 2], s2: [f64; 2] }`, `#[inline] process(&mut self, x: [f64; 2]) -> [f64; 2]`
  (TDF-II transposed), `set_coeffs`, `reset`.
- Tests: identity is passthrough; impulse response of a hand-computed filter matches to 1e-12;
  `reset` zeroes state.

**[x] T-1.2 Peaking filter design** — `src/design.rs`
- `fn peaking(fs: f64, f0: f64, q: f64, gain_db: f64) -> Coeffs` (RBJ cookbook).
  Returns `IDENTITY` if `gain_db == 0.0` or `f0 >= 0.45 * fs`.
- Tests: magnitude at f0 = `gain_db` ± 0.01 dB for fs ∈ {44 100, 48 000, 96 000} and gain ∈ {−12, −3, 3, 12};
  a 1 kHz band is within 0.1 dB of 0 at 10 Hz and at 0.49·fs.

**[x] T-1.3 Response curve** — `src/response.rs`
- `magnitude_db(&Coeffs, fs, f) -> f64`; `log_frequency(i, n, f_min, f_max) -> f64`;
  `fn response_curve(coeffs: &[Coeffs], fs, out: &mut [f32])` writing dB at `out.len()` log-spaced
  points from 20 Hz to 20 kHz (no allocation). `no_std`: use `libm` for `log10`/`sin`/`cos`.
- Tests: agrees with T-1.2; two identical +3 dB bands give +6 dB at f0.

**[x] T-1.4 Band layout** — `src/layout.rs`
- `enum Layout { Ten }` with `centres() -> &'static [f64]`, `q() -> f64` (√2), `len()`;
  constants `GAIN_MIN_DB = -12.0`, `GAIN_MAX_DB = 12.0`, `MAX_BANDS = 31` (capacity for later).
- `src/params.rs`: `#[derive(Clone, Copy, Debug, PartialEq)] EqParams { layout, gains: [f32; MAX_BANDS], enabled }`
  with `Default` (flat, enabled) and `band_gains(&self) -> &[f32]`. It lives here because both
  `eq-core` and `eq-engine` use it and neither depends on the other.
- Tests: 10 strictly increasing centres matching `architecture.md` §6.

**[x] T-1.5 Peak limiter** — `src/limiter.rs`
- `PeakLimiter::new(fs)`, stereo-linked, instant attack, ~100 ms release, ceiling −0.3 dBFS,
  `process(&mut self, x: [f64; 2]) -> [f64; 2]`, `set_sample_rate`.
- Tests: input up to +12 dBFS never exceeds the ceiling; signal at −6 dBFS is unchanged (± 1e-9).

**[x] T-1.6 Graphic EQ** — `src/graphic_eq.rs`
- `GraphicEq` holds `[StereoBiquad; MAX_BANDS]`, layout, fs, current/target gain per band, ramp
  state, `enabled` with a dry/wet crossfade, and a `PeakLimiter`.
- API: `new(fs)`, `set_sample_rate`, `set_gains(&[f32])` (clamped), `set_enabled(bool)`,
  `coeffs(&self) -> &[Coeffs]` (for the UI curve), `process_interleaved(&mut [f32])` (stereo).
- Behaviour per `architecture.md` §6.
- Tests: all-zero gains → bit-exact passthrough of a −6 dBFS signal; a 0 → +12 dB step on a 1 kHz
  sine has no sample-to-sample jump larger than the sine's maximum slope × 1.5; toggling enabled
  has no click (same check); **no allocation** in `process_interleaved` (`assert_no_alloc`).

**[x] T-1.7 Benchmark** — `benches/graphic_eq.rs` (criterion)
- 480-frame stereo block at 48 kHz, all 10 bands at non-zero gain, steady state and ramping.
- Acceptance: steady state < 50 µs per block on Windows and Linux; numbers in `docs/perf.md`.

## Phase 2 — Real-time engine (`crates/eq-engine`, platform-agnostic)

**[x] T-2.1 FIFO** — `src/fifo.rs`
- Pre-allocated interleaved-stereo `f32` ring: `with_capacity_frames(n)`, `push(&[f32]) -> usize`,
  `pop_into(&mut [f32]) -> usize`, `len_frames()`, `clear()`, `discard(frames)`, `pad_silence(frames)`.
  Single-threaded (Windows) plus `split()` into an `rtrb`-based SPSC producer/consumer pair
  (Linux: capture and playback callbacks).
- Tests: wrap-around correctness; SPSC pair under two threads keeps order; no allocation after construction.

**[x] T-2.2 Drift controller** — `src/drift.rs`
- PI controller (fill error in frames → ratio, clamped ±0.5 %) + `rubato` fixed-output resampler
  (`set_resample_ratio_relative(ratio, true)`); resync rule from `architecture.md` §5.1; counters.
- Tests: simulated producer 48 000 Hz vs consumer 48 010 Hz for 10 simulated minutes keeps fill
  within ±20 % of target, no resync after warm-up; equal clocks keep ratio at 1.0 ± 1e-4.

**[x] T-2.3 Params and stats handoff** — `src/shared.rs`
- `eq_dsp::EqParams` (defined in T-1.4) over `triple_buffer`
  (`ParamsWriter` / `ParamsReader`); `SharedStats` atomics (`latency_ms` as f32 bits,
  `underruns`, `resyncs`) with `snapshot() -> Stats`.
- Tests: writer/reader thread smoke test; reader sees the latest value.

**[x] T-2.4 Processor** — `src/processor.rs`
- `Processor::new(ProcessorConfig { sample_rate, out_channels, fifo_target_frames })` owning
  `GraphicEq`, drift controller and scratch buffers (all pre-allocated).
  `render(&mut self, fifo: &mut impl FrameSource, out: &mut [f32])`: read params once, pull
  resampled frames (silence + `underruns += 1` on shortage), EQ, up-mix stereo → `out_channels`
  (L/R → first two, others zero), publish stats.
- Tests: flat EQ + equal clocks → output equals input delayed; 6-channel output layout; no allocation.

**[x] T-2.5 Denormal guard** — `src/denormal.rs`
- `DenormalGuard` RAII: x86_64 sets FTZ|DAZ in MXCSR, aarch64 sets FZ in FPCR, restores on drop;
  no-op elsewhere. The only `unsafe` allowed in this crate, with `// SAFETY:` comments.
- Tests: on x86_64, a denormal multiply result is flushed to zero inside the guard.

## Phase 3 — Domain (`crates/eq-core`)

**[x] T-3.1 Model** — `src/model.rs`
- `DeviceId(String)`, `DeviceInfo { id, name, is_virtual }`, `LatencyMode { Low, Auto, Reliable }`
  (default `Auto`), `EqState { layout, gains: Vec<f32>, enabled }`, `Stats { latency_ms, underruns, resyncs }`.
  Serde derives where persisted.
- Tests: serde round-trip.

**[x] T-3.2 Ports and messages** — `src/ports.rs`, `src/messages.rs`
- `trait AudioBackend` (Abstract Factory) returning the parts below.
- `EndpointDirectory { list_render(), default_render(), set_default_render(&DeviceId) }`.
- `VirtualDeviceProvider { ensure(&[DeviceInfo]) -> Result<DeviceId, VirtualError>, release() }`
  (Windows: locate VB-CABLE; Linux: create/destroy the null sink).
- `PipelineFactory { start(PipelineConfig) -> Result<Box<dyn Pipeline>, PipelineError> }`,
  `Pipeline { set_eq(EqParams), stats() -> Stats, stop(self: Box<Self>) }`.
- `PipelineConfig { virtual_id, target_id, latency: LatencyMode, eq: EqParams }`.
- `DeviceEvent { DefaultChanged(DeviceId), Added(DeviceId), Removed(DeviceId), PipelineFailed(String) }`.
- `AppCommand { SetPower(bool), SetGain{band, db}, SetGains(Vec<f32>), SelectPreset(String),
  SavePreset, SavePresetAs(String), RenamePreset{from, to}, DeletePreset(String),
  SetLatency(LatencyMode), ShowWindow, Quit }`.
- `AppEvent { Snapshot(AppSnapshot), Stats(Stats), Error(String) }`.
- Acceptance: compiles on both OSes, every item documented, no OS types.

**[x] T-3.3 Router** — `src/router.rs`
- Pure `Router::handle(&mut self, RouterInput) -> Vec<Effect>` implementing `architecture.md` §7.
  `Effect { EnsureVirtual, ReleaseVirtual, SetDefault(DeviceId), StartPipeline{virtual_id, target_id},
  StopPipeline, PersistTarget(DeviceId), Status(RoutingStatus) }`.
- Tests: one per table row, plus — power on while default is already virtual uses the persisted
  target; `EnsureVirtual` failure → `Degraded(VirtualMissing)`; target removed with no other
  device → `Degraded(NoTarget)`.

**[x] T-3.4 Name-pattern locator** — `src/locator.rs`
- `PatternLocator { pattern }` (case-insensitive substring, default `"CABLE Input"`) — used by the
  Windows `VirtualDeviceProvider`.
- Tests: finds it; ignores "CABLE Output"; returns `None` when absent.

**[x] T-3.5 Presets** — `src/presets.rs`
- Built-ins (our own 10-band values): Flat, Bass Boost, Bass Reducer, Treble Boost, Vocal, Rock,
  Electronic, Acoustic, Loudness.
- `PresetStore` over a directory: `list()`, `get(name)`, `save(preset)`, `rename(from, to)`,
  `delete(name)`; one JSON file per user preset; unique case-insensitive names; cross-platform
  file-name sanitising per `architecture.md` §8; built-ins read-only.
- Tests (temp dir): CRUD round-trip; duplicate name rejected; built-in protection; invalid JSON
  file skipped with an error entry; Windows-reserved names (`CON`, `nul.txt`) rejected on all OSes.

**[x] T-3.6 Settings** — `src/settings.rs`
- `Settings` per `architecture.md` §9; paths via `directories`; TOML load/save; atomic write;
  corrupt file → `.bak` + defaults.
- Tests: round-trip; corrupt-file recovery; missing file → defaults.

**[x] T-3.7 Controller** — `src/controller.rs`
- Own thread; `crossbeam_channel::select!` over `AppCommand` and `DeviceEvent`; feeds the router
  and executes effects through the ports; debounces `DefaultChanged` (150 ms); tracks active
  preset + modified flag; pushes `EqParams` on every gain change; polls `stats()` 4×/s →
  `AppEvent::Stats`; saves settings debounced 500 ms; restarts the pipeline on `SetLatency` and on
  `PipelineFailed` (backoff 100 ms, 500 ms, 2 s, then `Degraded(PipelineFailed)`).
- Tests with fake ports: power on/off call sequence (incl. ensure/release virtual); default change
  restarts once after three events; editing a gain sets modified; selecting a preset sends its
  gains; latency change restarts with the new mode.

## Phase 4 — Windows backend (`crates/eq-wasapi`)

**[x] T-4.1 COM utilities** — `src/com.rs`, `src/error.rs`
- `ComApartment` RAII (`CoInitializeEx` MTA ↔ `CoUninitialize`); `WasapiError` (thiserror) with
  context; `pwstr_to_string` that frees with `CoTaskMemFree`.
- Acceptance: test creates and drops the apartment twice on one thread.

**[x] T-4.2 Device directory + virtual provider** — `src/devices.rs`, `src/virtual_device.rs`
- `MmDirectory`: `list_render`, `default_render` (`eConsole`) via `IMMDeviceEnumerator` and
  `PKEY_Device_FriendlyName`. `CableProvider` implementing `VirtualDeviceProvider` with `PatternLocator`.
- Example `examples/list_devices.rs`.
- Acceptance (Windows): prints the same devices as the sound flyout; marks CABLE Input as virtual.

**[x] T-4.3 Set default endpoint** — `src/policy_config.rs`
- Declare `IPolicyConfig` (IID `F8679F50-850A-41CF-9C72-430F290290C8`, CLSID
  `870AF99C-171D-4F9E-AF0D-E63DF40C2BC9`) with `#[windows::core::interface]` and the exact vtable
  order: GetMixFormat, GetDeviceFormat, ResetDeviceFormat, SetDeviceFormat, GetProcessingPeriod,
  SetProcessingPeriod, GetShareMode, SetShareMode, GetPropertyValue, SetPropertyValue,
  SetDefaultEndpoint, SetEndpointVisibility. Fallback `IPolicyConfigVista`
  (IID `568B9108-44BF-40B4-9006-86AFE5B5A620`). `set_default_render` for `eConsole` + `eMultimedia`.
  `RestoreDefaultGuard` restores the previous default on drop.
- Example `examples/set_default.rs <name-substring>` (restores on Enter).
- Acceptance (Windows): the flyout shows the switch and the restore.

**[x] T-4.4 Device notifications** — `src/notifications.rs`
- `#[implement(IMMNotificationClient)]` holding `Sender<DeviceEvent>`; callbacks only convert and
  send. `NotificationGuard` registers/unregisters. Only `eRender`.
- Example `examples/watch_devices.rs`.
- Acceptance (Windows): switching device in the flyout prints `DefaultChanged`.

**[~] T-4.5 WASAPI pipeline** — `src/pipeline.rs`
- RT thread: `ComApartment`, MMCSS guard (`AvSetMmThreadCharacteristicsW("Pro Audio")`),
  `DenormalGuard`. Clients per `architecture.md` §5.1; period per latency mode.
- Loop: `WaitForMultipleObjects([render_event, stop_event])` → drain capture packets into the
  FIFO → `frames = buffer_size − padding` → `Processor::render` into the render buffer →
  `ReleaseBuffer`. Latency = `GetStreamLatency` (both) + FIFO + padding.
  `AUDCLNT_E_DEVICE_INVALIDATED` / format change → thread exits, sends `PipelineFailed`.
- Acceptance (Windows): with CABLE Input as default and flat EQ, audio plays on the target for
  10 minutes with no artefacts; **record in `docs/perf.md` whether changing CABLE Input's volume
  changes loudness** (architecture §12).

**[~] T-4.6 Backend + CLI harness** — `src/backend.rs`, `examples/route_cli.rs`
- `WasapiBackend` implementing `AudioBackend`. `route_cli --target <substr> [--latency low|auto|reliable]`
  runs the real controller without UI, prints stats every second, restores the default on Ctrl+C.
- Acceptance (Windows): switching device in the flyout moves audio and flips the default back to
  CABLE Input within 300 ms; disabling the target in Sound settings recovers.

## Phase 5 — Linux backend (`crates/eq-pipewire`)

**[ ] T-5.1 PipeWire loop thread** — `src/pw_thread.rs`, `src/error.rs`
- Dedicated thread owning `MainLoop`, `Context`, `Core`, `Registry`; commands in via
  `pipewire::channel`, results/events out via crossbeam. Clean shutdown on drop.
  Connection failure → typed `PipeWireError::NotRunning`.
- Acceptance: unit test (skipped when no PipeWire) connects and shuts down; WSL build passes.

**[ ] T-5.2 Device directory + events** — `src/devices.rs`
- Registry listener tracking nodes with `media.class = Audio/Sink`: id = `node.name`,
  name = `node.description`, `is_virtual` for `equalizerer_sink`. Emits `Added`/`Removed`.
- Default sink via the `default` metadata object: read `default.audio.sink` (JSON `{"name": …}`),
  emit `DefaultChanged` on change; `set_default_render` writes `default.configured.audio.sink`.
- Example `examples/list_devices.rs` and `examples/watch_devices.rs` (Linux variants).
- Acceptance (Linux desktop): list matches `wpctl status`; `wpctl set-default <id>` prints
  `DefaultChanged`; `set_default_render` changes the default shown in the desktop sound settings.

**[ ] T-5.3 Virtual sink provider** — `src/virtual_sink.rs`
- `NullSinkProvider` implementing `VirtualDeviceProvider`: create the node per
  `architecture.md` §5.2 (plus `monitor.channel-volumes = true`), keep the proxy alive while in
  use, destroy on `release()`/drop.
- Acceptance (Linux desktop): "Equalizerer" appears as an output in sound settings while held and
  disappears on release and when the process is killed.

**[ ] T-5.4 PipeWire pipeline** — `src/pipeline.rs`
- Capture stream (`stream.capture.sink = true`, `target.object = equalizerer_sink`) and playback
  stream (`target.object = <target>`, `node.dont-reconnect = true`), float32, `node.latency` per
  latency mode, `RT_PROCESS` flag. Capture callback → FIFO producer; playback callback →
  `Processor::render` (FIFO consumer). Latency from `pw_stream_get_time_n` of both streams + FIFO.
  Stream error/unconnected state → `PipelineFailed`.
- Acceptance (Linux desktop): with "Equalizerer" as default and flat EQ, audio from a browser plays
  on the target for 10 minutes without artefacts; `pw-top` shows no xruns growth; changing the
  system volume changes loudness.

**[ ] T-5.5 Backend + CLI harness** — `src/backend.rs`, `examples/route_cli.rs`
- `PipeWireBackend` implementing `AudioBackend`; same CLI as T-4.6.
- Acceptance (Linux desktop): changing output in sound settings moves audio to the new device and
  the default returns to "Equalizerer" within 300 ms; unplugging the target recovers.

## Phase 6 — App (`crates/eq-app`)

**[~] T-6.1 Composition root** — `src/main.rs`, `src/backend.rs`, `src/single_instance.rs`
- `cfg`-selected `AudioBackend` (the only place that names a backend). Args: `--restore`.
  Single instance per `architecture.md` §10 (second launch sends `ShowWindow` and exits).
  `tracing` to the platform log dir (daily rolling). Panic hook restores the default device.
  Quit = power-off effects, persisted power state kept.
- Acceptance (both OSes): `--restore` restores the persisted target and exits; kill + relaunch
  recovers; second launch focuses the first window.

**[x] T-6.2 UI shell, theme and power button** — `ui/app.slint`, `ui/theme.slint`, `src/ui/mod.rs`
- Theme tokens, platform font (Segoe UI Variable / bundled Inter), header with power toggle and
  status line per `architecture.md` §10. Bridge `AppEvent` → `slint::invoke_from_event_loop`;
  callbacks → `AppCommand`.
- Acceptance (both OSes; Linux on X11 and Wayland): power toggle routes/unroutes audio; status
  shows target name or the platform-specific hint.

**[x] T-6.3 10-band EQ widget** — `ui/eq.slint`, `src/ui/eq_view.rs`
- 10 vertical sliders ±12 dB (0.1 dB steps, double-click resets, wheel = 0.5 dB), value labels,
  frequency labels, dB grid, filled response curve from `eq-dsp::response_curve` (recomputed only
  when gains change).
- Acceptance: dragging changes the sound smoothly with no clicks; curve matches.

**[x] T-6.4 Presets UI** — `ui/presets.slint`, `src/ui/presets_view.rs`
- Dropdown with "Built-in" and "My presets", modified marker, Save / Save as / Rename / Delete
  (confirm dialog), validation messages.
- Acceptance: CRUD works and survives restart on both OSes; built-ins cannot be overwritten.

**[x] T-6.5 Latency UI** — `ui/latency.slint`
- Three-detent slider, measured latency in ms, hint when underruns grow in Lower Latency.
- Acceptance: changing mode restarts audio within ~200 ms and the displayed ms changes.

**[ ] T-6.6 Polish**
- Animations, keyboard navigation, HiDPI (Windows per-monitor v2 manifest; Wayland fractional
  scaling), icons: Windows `.ico` + version resource (`embed-resource`), Linux `.desktop` file +
  PNG/SVG icons under `packaging/linux/`.

**[x] T-6.7 Tray, minimize to tray, start with Windows** — `src/tray.rs`, `src/icon.rs`,
`src/ui/window.rs`, `eq-wasapi/src/autostart.rs`
- Tray icon with "Open Equalizerer" and "Quit" (double-click or left click opens). Minimize hides
  the window to the tray; close quits; `--minimized` starts hidden. No maximize button. "Start with
  Windows" checkbox below the audio card writes the HKCU Run value with `--minimized`.
- Acceptance (Windows): tray opens and quits the app; minimize removes the window from the
  taskbar; a second launch shows the hidden window; the checkbox toggles the Run value. Linux: no
  tray, the window keeps the ordinary behaviour.

## Phase 7 — QA

**[ ] T-7.1 Scenario checklist** — `docs/testing.md`, run on Windows and on a Linux desktop:
power on/off; switch device in OS settings; unplug/replug headphones; sleep/resume; each latency
mode; target sample-rate change; quit restores default; kill + `--restore`; (Linux) restart
PipeWire with `systemctl --user restart pipewire` → app recovers or shows the hint.

**[ ] T-7.2 Soak test** — 8 h playback in Automatic on each OS; CPU, RAM, underruns, resyncs in
`docs/perf.md`.

---

## Backlog (not in MVP)

| Item | Notes |
|---|---|
| 31-band layout | `Layout::ThirtyOne` (ISO third-octave, Q ≈ 4.318) + 10/31 toggle |
| Pre-amp + auto pre-amp, clip LED, output meter | |
| Output device list in the app | Select target from the UI |
| Linux tray icon and XDG autostart | Windows tray and Run key are done (T-6.7); Linux needs a tray (StatusNotifierItem) and an autostart `.desktop` file |
| AutoEq import / preset export | AutoEq TSV and `GraphicEQ:` text |
| Windows volume bridge | **Promote to MVP if T-4.5 shows volume keys don't work** |
| Idle auto-stop | Stop the pipeline when nothing plays; measure first |
| PulseAudio-only Linux | `module-null-sink` + libpulse backend |
| Linux aarch64 | Build + test on ARM |
| macOS | CoreAudio backend (needs a virtual device such as BlackHole) |
| Own Windows virtual driver | SysVAD fork; needs signing — research spike first |
| Sweep measurement tool | Verify the EQ curves against measurements |
| Packaging | Windows zip/MSI; Linux AppImage/Flatpak/.deb; code signing |
