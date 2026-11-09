# Architecture

Scope of this document: the **MVP** — power button, 10-band EQ with multiple presets, latency
system — on **Windows and Linux**, plus the extension points kept open for the backlog.

Priorities, in order: **glitch-free audio → maximum performance → clean architecture → modern UI**.

## 1. Big picture

```
 Apps ──► Virtual device (system default) ──capture──► Equalizerer ──► Target device (real)
                                                        EQ + latency
```

The same routing trick on both platforms; only the OS plumbing differs.

| | Windows | Linux |
|---|---|---|
| Audio API | WASAPI + MMDevice (COM) | PipeWire (native API; also serves PulseAudio/JACK apps via pipewire-pulse) |
| Virtual device | **VB-CABLE** "CABLE Input", installed once by the user, located by name | **Created by the app at runtime**: a PipeWire null sink "Equalizerer", removed when the app exits — no driver, no install |
| Capture | WASAPI loopback on the virtual render endpoint | Capture stream on the virtual sink's monitor |
| Set default output | Undocumented `IPolicyConfig::SetDefaultEndpoint` | PipeWire `default` metadata, key `default.configured.audio.sink` |
| Default-changed events | `IMMNotificationClient` | Metadata listener on `default.audio.sink` |
| Device list / removal | `IMMDeviceEnumerator` | Registry listener (`Audio/Sink` nodes) |
| RT thread priority | MMCSS "Pro Audio" | PipeWire data thread (RT via rtkit) |
| Clock drift | Our drift resampler (two independent WASAPI clocks) | Handled by the PipeWire graph (adaptive resampling of followers); our resampler stays at ratio 1.0 |

- **Target device:** the real output the user wants to hear. When the user picks another output
  in the OS (Windows sound flyout; GNOME/KDE sound settings or `wpctl set-default`), that device
  becomes the new target and the virtual device is made default again.
- **Power off** stops processing and makes the target the default again.
- Linux support targets **PipeWire** (default on Fedora, Ubuntu ≥ 22.10, Debian 12, Arch, …).
  PulseAudio-only systems are backlog.

## 2. Patterns

- **Hexagonal (ports & adapters).** `eq-core` defines traits (ports) for what it needs from the
  OS; `eq-wasapi` and `eq-pipewire` implement them; tests use fakes. Only `eq-app` knows which
  backend exists, selected at compile time with `#[cfg(target_os = …)]`.
- **Functional core, imperative shell.** DSP and the router state machine are pure. The router
  receives inputs and returns *effects*; the controller executes them.
- **Actors.** Long-lived threads talk only through messages (no shared mutable state).
- **State** (router), **Strategy** (virtual-device provider, latency mode), **Abstract Factory**
  (`AudioBackend` bundling one platform's adapters), **Builder** (pipeline config),
  **RAII guards** (COM apartment, MMCSS, notification registration, virtual sink lifetime,
  restore-default-on-drop).

## 3. Crates

| Crate | Responsibility | Platform | Depends on |
|---|---|---|---|
| `eq-dsp` | Biquads, filter design, 10-band graphic EQ, smoothing, limiter, response curve | any (`#![no_std]`) | — |
| `eq-engine` | Platform-agnostic real-time building blocks: FIFO, drift controller + resampler, processor (EQ + up-mix + metering), param/stat handoff, denormal guard | any | `eq-dsp`, rubato, triple_buffer |
| `eq-core` | Domain model, ports, router, presets, settings, controller | any | `eq-dsp`, serde, toml, crossbeam-channel, thiserror |
| `eq-wasapi` | Windows backend | `cfg(windows)` | `eq-core`, `eq-engine`, `windows` |
| `eq-pipewire` | Linux backend | `cfg(target_os = "linux")` | `eq-core`, `eq-engine`, `pipewire` |
| `eq-app` | Composition root, backend selection, Slint UI, single instance, logging | any | all of the above (backends via target-specific dependencies), slint, anyhow, tracing |

Dependency direction: `eq-app → {eq-wasapi | eq-pipewire} → {eq-core, eq-engine} → eq-dsp`.
`eq-dsp`, `eq-engine` and `eq-core` never depend on OS-specific crates.

## 4. Threads

```
 ┌──────────── UI thread (Slint) ────────────┐
 │ renders state, sends AppCommand           │
 └───────▲──────────────────────┬────────────┘
         │ AppEvent             │ AppCommand            (crossbeam channels)
 ┌───────┴──────────────────────▼────────────┐
 │ Controller thread                         │◄── DeviceEvent from the backend
 │ router, settings, presets                 │    (OS callbacks only forward events)
 └───────┬──────────────────────▲────────────┘
         │ EqParams             │ Stats
         │ (triple buffer)      │ (atomics)
 ┌───────▼──────────────────────┴────────────┐
 │ Audio RT context                          │
 │ Windows: our thread, MMCSS                │
 │ Linux:   PipeWire data-loop callbacks     │
 │ capture → FIFO → resampler → EQ → render  │
 └───────────────────────────────────────────┘
```

Backend-internal threads: on Windows the controller thread owns the COM apartment; on Linux a
dedicated PipeWire main-loop thread owns the PipeWire core/registry/metadata, and the controller
talks to it through `pipewire::channel`.

## 5. Audio pipeline (hot path)

Shared logic lives in `eq-engine::Processor`: it takes captured frames into a pre-allocated FIFO
and fills a render buffer of N channels (`drift resampler → GraphicEq → up-mix`), updating stats.
Each backend only moves buffers in and out.

### 5.1 Windows (`eq-wasapi`)

- **Clock master = target device.** One RT thread waits on the target's render event. On each
  wake-up it drains all available loopback packets from the virtual device into the FIFO, then
  produces exactly the frames the render buffer asks for. (Event-driven loopback capture is
  unreliable, so capture is polled from the render loop.)
- **Capture:** `IAudioClient`, shared, `AUDCLNT_STREAMFLAGS_LOOPBACK | AUTOCONVERTPCM |
  SRC_DEFAULT_QUALITY` on the virtual render endpoint, float32 stereo at the target's sample
  rate — Windows converts format and rate, we only correct clock drift. Silent packets become zeros.
- **Render:** `IAudioClient3`, shared, event-driven, target mix format (float32).
- **Drift:** a PI controller keeps the FIFO fill at the latency target by adjusting a `rubato`
  fixed-output resampler ratio within ±0.5 %. Fill outside `[0, 3 × target]` → resync + counter.

### 5.2 Linux (`eq-pipewire`)

- **Virtual sink:** created with the `adapter` factory, `factory.name = support.null-audio-sink`,
  `media.class = Audio/Sink`, `node.name = equalizerer_sink`, `node.description = Equalizerer`,
  `audio.position = [FL FR]`, `object.linger = false` (PipeWire removes it if the app dies).
- **Capture stream:** `pw_stream` with `target.object = equalizerer_sink`,
  `stream.capture.sink = true` (records the sink's monitor), float32 stereo, `node.passive = true`.
- **Playback stream:** `pw_stream` with `target.object = <target node.name>`,
  `node.dont-reconnect = true` (the router decides where audio goes), float32, target channels.
- Both streams run in the PipeWire graph; their `process` callbacks run on the data loop.
  The capture callback pushes into the FIFO, the playback callback calls `Processor::render`.
  The graph resamples followers to the driver clock, so the drift controller normally stays at 1.0;
  it is kept active as a safety net.
- `node.latency` on both streams sets the quantum (see §5.3).

### 5.3 Latency system

The latency mode sets the device/graph period and the FIFO target fill.

| Mode (UI label) | Windows render period | Linux `node.latency` (at 48 kHz) | FIFO target | Typical end-to-end |
|---|---|---|---|---|
| Lower Latency | engine minimum via `IAudioClient3` (often 3–10 ms) | `256/48000` (5.3 ms) | 1.5 × period | ~10–20 ms |
| Automatic (default) | 10 ms | `1024/48000` (21 ms) | 30 ms | ~40–60 ms |
| More Reliable | 10 ms | `2048/48000` (43 ms) | 80 ms | ~90–130 ms |

- PipeWire may run a larger quantum than requested if another client asks for more; the
  measured value is what the UI shows.
- Changing the mode restarts the pipeline (short gap is acceptable).
- **Measured latency** = capture-side delay + FIFO fill + render-side delay
  (Windows: `GetStreamLatency` + padding; Linux: `pw_stream_get_time_n` delay of both streams),
  reported through `Stats.latency_ms` ~4×/s, together with `underruns` and `resyncs`.
  If underruns keep growing in Lower Latency the UI suggests Automatic.

### 5.4 Real-time rules

No allocation, locks, logging or blocking in RT code (see `AGENTS.md`). Denormals: FTZ/DAZ via
MXCSR on x86_64, FZ bit in FPCR on aarch64 — wrapped in `eq-engine::DenormalGuard`.

## 6. DSP

- **Biquad:** Transposed Direct Form II. Coefficients and state in `f64`, audio I/O in `f32`.
  Stereo is one `[f64; 2]` lane per filter (auto-vectorises to SSE2/NEON).
- **Filter design:** RBJ Audio EQ Cookbook peaking filter.
- **10-band layout:** 32, 63, 125, 250, 500, 1k, 2k, 4k, 8k, 16k Hz; Q = √2 (one octave);
  gains ±12 dB in 0.1 dB steps. `Layout` is an enum (only `Ten` now). Bands with centre ≥ 0.45·fs
  are disabled.
- **Processing order:** bands → limiter. Bands at 0 dB that are not ramping are skipped.
- **Smoothing:** gain changes ramp linearly in dB over ~30 ms; ramping bands recompute their
  coefficients every 32 frames → no zipper noise.
- **Power toggle inside the DSP:** disabling crossfades to the dry signal over one ramp — no clicks.
- **Limiter:** stereo-linked peak limiter, instant attack, ~100 ms release, ceiling −0.3 dBFS.
- **Accurate mode (band-interaction correction):** neighbouring peaking bands overlap, so the
  summed response at a band centre differs from its slider (+10 dB at 32 Hz next to −12 dB at
  63 Hz gives only about +7.5 dB at 32 Hz). `corrected_gains` (`eq-dsp/src/correction.rs`, no
  allocation) runs `ITERATIONS` = 8 undamped fixed-point steps: measure the summed response at
  every centre, move each band by the error at its own centre, clamp to ±18 dB. `GraphicEq` keeps
  the sliders as given and runs the bands at the corrected gains, and `coeffs()` returns those.
  The correction is recomputed only when a slider changes, when accurate mode is toggled or when
  the sample rate changes. Always on in the MVP (`GraphicEq::set_accurate` exists for tests and
  future use). Measured error: random sliders within ±12 dB stay within 0.32 dB at every centre;
  alternating extremes (+12/−12 on neighbours) need more than the 18 dB cap, so they stay up to
  about 1.2 dB off.
- **Response curve:** computed from the same coefficients (`|H(e^jω)|` at 128 log-spaced points
  from the first band centre, 32 Hz, to the last, 16 kHz). The UI draws it in real logical pixels
  (the path viewbox always equals the plot size, recomputed on resize), and the band sliders sit
  on the same log axis, so the first slider is at the left edge of the curve and the last at the
  right edge. The curve is drawn from the corrected gains, so it passes through the slider
  handles: at each band centre it shows that band's slider value.

## 7. Routing state machine (platform-independent)

States: `Off`, `On { virtual_id, target_id }`, `Degraded { reason }`
(`VirtualMissing`, `NoTarget`, `PipelineFailed`).

| Input | Effects |
|---|---|
| `PowerOn` | `EnsureVirtual` (Windows: locate; Linux: create sink) → target = current default if not virtual, else persisted target → `SetDefault(virtual)` → `StartPipeline(virtual, target)` |
| `DefaultChanged(id)`, id ≠ virtual, while On | target = id → `StopPipeline` → `StartPipeline(virtual, id)` → `SetDefault(virtual)` → persist target |
| `DefaultChanged(virtual)` | ignored (our own change) |
| `DeviceRemoved(target)` while On | target = previous target or first active non-virtual device → restart; `NoTarget` if none |
| `DeviceRemoved(virtual)` | `StopPipeline`, `Degraded(VirtualMissing)` |
| `PowerOff` | `StopPipeline` → `SetDefault(target)` → `ReleaseVirtual` (Linux: destroy sink; Windows: no-op) |
| App quit | same as `PowerOff`, but the persisted power state stays "on" |

- The controller debounces `DefaultChanged` by 150 ms (Windows sends one per role).
- Windows roles set on the virtual device: console and multimedia.
- Crash safety: the target is persisted; a panic hook restores the default; `eq-app --restore`
  restores it without the UI. On Linux the sink also disappears with the process and WirePlumber
  falls back to another sink automatically.

## 8. Presets

- A preset = name + layout + 10 gains. Built-in presets (our own values, read-only):
  Flat, Bass Boost, Bass Reducer, Treble Boost, Vocal, Rock, Electronic, Acoustic, Loudness.
- User presets: save as, rename, delete, select. One JSON file per preset in
  `<config dir>/presets/`.
- Moving a slider after selecting a preset marks it **modified**; "Save" overwrites a user
  preset, "Save as" creates a new one. Built-ins can only be saved as a new preset.
- Names are unique (case-insensitive) and sanitised for file names on both OSes
  (reject `<>:"/\|?*`, control characters, trailing dots/spaces, Windows reserved names).

## 9. Settings and paths

Paths come from the `directories` crate:

| | Windows | Linux |
|---|---|---|
| Config (`settings.toml`, `presets/`) | `%APPDATA%\equalizerer\config\` | `$XDG_CONFIG_HOME/equalizerer/` (`~/.config/equalizerer/`) |
| Logs | `%LOCALAPPDATA%\equalizerer\data\logs\` | `$XDG_STATE_HOME/equalizerer/logs/` (`~/.local/state/…`) |

`settings.toml`, versioned: `version, power_on, active_preset, current_gains, latency_mode,
target_device_id, virtual_device_pattern` (Windows only). Device ids are backend-specific
strings (Windows endpoint id / PipeWire `node.name`). Atomic write (temp + rename), debounced
500 ms; a corrupt file is renamed to `.bak` and defaults are used.

## 10. UI (Slint)

Slint runs natively on both platforms (winit; Windows, X11 and Wayland). Dark theme in the
spirit of the reference screenshots. Font: Segoe UI Variable on Windows, Inter (bundled) on
Linux. MVP window, top to bottom:

1. **Header:** app name, **power button**, status line ("Output: LG ULTRAWIDE" /
   "Virtual device not found — install VB-CABLE" on Windows / "PipeWire not running" on Linux).
2. **Equalizer card:** preset dropdown (Built-in / My presets, "modified" marker, Save,
   Save as, Rename, Delete), 10 vertical sliders with value labels, filled response curve,
   frequency labels, dB grid. Double-click resets a slider to 0; mouse wheel = 0.5 dB.
3. **Audio processing card:** latency slider with three detents and the measured latency in ms.
4. **Start with Windows** checkbox below that card (Windows only; hidden elsewhere). Checking it
   writes the value `Equalizerer` under `HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`
   with the data `"<exe path>" --minimized`; unchecking deletes it. The state is read from that
   value at start. Demo mode shows the checkbox but writes nothing.

Redraws only on change; the latency label refreshes from `Stats` ~4×/s.

**Window and tray (Windows).** The window stays resizable but has no maximize button: the
maximize box is removed, so it cannot be used, and Windows still draws it greyed out because the
window can be resized. The minimize and close buttons remain.

- **Minimize** hides the window to the tray: it leaves the taskbar and audio keeps running. The
  window's native settings are applied with Slint's winit accessor (`unstable-winit-030`); Slint
  re-enables the maximize box when it creates the window, so the event filter removes it once the
  window exists.
- **Close (X)** quits the app, the same as the tray's "Quit": the controller powers off and
  restores the default device.
- **Tray icon** (`src/tray.rs`, `tray-icon` crate): a 32 × 32 icon generated in code
  (`src/icon.rs`, accent square with four EQ bars), tooltip "Equalizerer". Double-click or a left
  click opens the window; the right-click menu has "Open Equalizerer" and "Quit".
- **Showing again:** the tray opens the window, and a second launch of the exe asks the running
  instance to show its window. Both un-minimize and bring the window to the front.
- **`--minimized`** starts with the window hidden, so only the tray icon is visible. The autostart
  entry passes it. Without a tray icon the window is shown at start and minimizes normally.
- The event loop runs until a quit, so hiding the window does not end the program.

Linux has no tray and no autostart yet (backlog): the window behaves as an ordinary window there.

Single instance: named mutex on Windows, lock file + Unix socket in `$XDG_RUNTIME_DIR` on Linux;
a second launch asks the first to show its window and exits.

## 11. Performance budget (both platforms)

| Metric | Target |
|---|---|
| CPU while playing (10 bands, 48 kHz stereo) | < 0.5 % of one core |
| DSP time per 480-frame block | < 50 µs |
| Allocations in RT code | 0 (asserted) |
| RAM (app + UI) | < 60 MB |
| Default-device switch → audio on new target | < 300 ms |
| Underruns over 8 h in Automatic | no growth after warm-up |

## 12. Known risks

- **Windows volume keys:** whether loopback on the virtual device is captured before or after its
  endpoint volume decides if volume keys work without extra code (checked in T-4.5). If not,
  the volume bridge moves from backlog to MVP. On Linux the desktop volume control changes the
  default sink (our virtual sink), whose volume PipeWire applies before the monitor only if
  `monitor.channel-volumes = true`; this is set on the sink so volume keys keep working.
- **Linux test environment:** audio behaviour must be verified on a real Linux desktop with
  PipeWire (native or VM with audio). WSL2 is enough for building and unit tests only.
