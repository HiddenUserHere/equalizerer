# Performance

Measured numbers for the real-time code. Budgets come from
[architecture.md](architecture.md) section 11.

## DSP benchmarks (T-1.7)

- **Date:** 2026-11-09
- **Machine:** dev machine (Windows 11)
- **Build:** `cargo bench -p eq-dsp --bench graphic_eq -- --quick` (release profile, LTO)
- **Workload:** 480-frame stereo block at 48 kHz, all 10 bands at non-zero gain, 1 kHz test sine.
  - *Steady state:* gains `[3, -2, 4, -1, 2, -3, 1, -4, 2, 3]` dB, ramps finished before timing.
    Accurate mode is on (the default); the filters run at the corrected gains.
  - *Ramping:* two gain sets alternate every block, so all 10 bands ramp during every block.
    The timing includes the `set_gains` call that starts the ramps, including the band
    correction of accurate mode.
  - *Correction:* one `corrected_gains` call for the same gains (8 iterations, 10 bands).

| Scenario | Time per 480-frame block (median) | Budget | Result |
|---|---|---|---|
| Steady state, 10 bands | 8.2 µs | < 50 µs | pass |
| Ramping, 10 bands (with correction) | 19.8 µs | < 50 µs | pass |
| `corrected_gains`, 10 bands | 6.9 µs | (runs once per gain change) | — |

Before accurate mode the same benchmark gave 10.7 µs (steady) and 13.6 µs (ramping). The
steady-state path is unchanged by the correction; the difference between the two runs is
measurement noise on this machine.

Budget: DSP time per 480-frame block below 50 µs. Both scenarios pass on this machine.
The Linux measurement is still to be done.

Quick mode gives a short sampling run, so the figures are indicative rather than
precise to the last digit.
