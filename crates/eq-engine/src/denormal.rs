//! RAII guard that makes the current thread flush denormal floats to zero.
//!
//! Denormal (subnormal) operands and results make many CPUs run floating-point code 10 to 100
//! times slower. Filter states decaying towards silence produce exactly such values, so the
//! audio thread runs inside a [`DenormalGuard`]:
//!
//! - `x86_64`: FTZ (bit 15) and DAZ (bit 6) in `MXCSR`;
//! - aarch64: FZ (bit 24) in `FPCR`;
//! - other architectures: no-op.
//!
//! The previous control register value is restored on drop. The guard changes per-thread CPU
//! state, so it is neither `Send` nor `Sync`.

use core::marker::PhantomData;

/// Keeps the thread's floating-point control register in flush-to-zero mode until dropped.
///
/// Create it at the start of the audio thread's loop (or callback) and keep it alive for as
/// long as that code runs.
#[must_use = "the previous FP state is restored as soon as the guard is dropped"]
#[derive(Debug)]
pub struct DenormalGuard {
    #[cfg(target_arch = "x86_64")]
    previous_mxcsr: u32,
    #[cfg(target_arch = "aarch64")]
    previous_fpcr: u64,
    /// Makes the guard `!Send` and `!Sync`: it must be dropped on the thread that created it.
    _thread_bound: PhantomData<*const ()>,
}

#[cfg(target_arch = "x86_64")]
impl DenormalGuard {
    /// `MXCSR.FTZ`: results that would be denormal are flushed to zero.
    const FTZ: u32 = 1 << 15;
    /// `MXCSR.DAZ`: denormal inputs are treated as zero.
    const DAZ: u32 = 1 << 6;

    /// Enables FTZ and DAZ on the current thread.
    #[allow(
        clippy::new_without_default,
        reason = "creating the guard changes CPU state, so it must be explicit; no Default"
    )]
    pub fn new() -> Self {
        let previous_mxcsr = read_mxcsr();
        write_mxcsr(previous_mxcsr | Self::FTZ | Self::DAZ);
        Self {
            previous_mxcsr,
            _thread_bound: PhantomData,
        }
    }
}

#[cfg(target_arch = "x86_64")]
impl Drop for DenormalGuard {
    fn drop(&mut self) {
        write_mxcsr(self.previous_mxcsr);
    }
}

/// Reads the `MXCSR` register of the current thread.
#[cfg(target_arch = "x86_64")]
fn read_mxcsr() -> u32 {
    let mut value: u32 = 0;
    // SAFETY: `stmxcsr` only stores the 32-bit MXCSR register to the given memory location.
    // `value` is a live, aligned `u32` on this stack frame, and the instruction does not touch
    // the stack or the flags.
    unsafe {
        core::arch::asm!(
            "stmxcsr [{ptr}]",
            ptr = in(reg) core::ptr::addr_of_mut!(value),
            options(nostack, preserves_flags),
        );
    }
    value
}

/// Writes `value` to the `MXCSR` register of the current thread.
#[cfg(target_arch = "x86_64")]
fn write_mxcsr(value: u32) {
    // SAFETY: `ldmxcsr` loads the 32-bit MXCSR register from the given memory location. The
    // pointed-to `u32` is a live, aligned local for the whole instruction. Only floating-point
    // control bits are changed by this crate, and the caller passes values derived from a
    // previous `read_mxcsr` (reserved bits are preserved).
    unsafe {
        core::arch::asm!(
            "ldmxcsr [{ptr}]",
            ptr = in(reg) core::ptr::addr_of!(value),
            options(nostack, preserves_flags),
        );
    }
}

#[cfg(target_arch = "aarch64")]
impl DenormalGuard {
    /// `FPCR.FZ`: denormal inputs and results are flushed to zero.
    const FZ: u64 = 1 << 24;

    /// Enables flush-to-zero on the current thread.
    #[allow(
        clippy::new_without_default,
        reason = "creating the guard changes CPU state, so it must be explicit; no Default"
    )]
    pub fn new() -> Self {
        let previous_fpcr = read_fpcr();
        write_fpcr(previous_fpcr | Self::FZ);
        Self {
            previous_fpcr,
            _thread_bound: PhantomData,
        }
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for DenormalGuard {
    fn drop(&mut self) {
        write_fpcr(self.previous_fpcr);
    }
}

/// Reads the `FPCR` register of the current thread.
#[cfg(target_arch = "aarch64")]
fn read_fpcr() -> u64 {
    let value: u64;
    // SAFETY: `mrs` reads the system register FPCR into a general-purpose register. It has no
    // memory or flag side effects.
    unsafe {
        core::arch::asm!(
            "mrs {value}, fpcr",
            value = out(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
    value
}

/// Writes `value` to the `FPCR` register of the current thread.
#[cfg(target_arch = "aarch64")]
fn write_fpcr(value: u64) {
    // SAFETY: `msr` writes the system register FPCR from a general-purpose register. Callers
    // pass values derived from `read_fpcr`, so only the FZ bit changes. The instruction has no
    // memory or flag side effects.
    unsafe {
        core::arch::asm!(
            "msr fpcr, {value}",
            value = in(reg) value,
            options(nomem, nostack, preserves_flags),
        );
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
impl DenormalGuard {
    /// No-op on this architecture.
    #[allow(
        clippy::new_without_default,
        reason = "keeps the same API on every architecture; no Default"
    )]
    pub fn new() -> Self {
        Self {
            _thread_bound: PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn guard_flushes_denormal_products_and_restores_state() {
        const FTZ_DAZ: u32 = DenormalGuard::FTZ | DenormalGuard::DAZ;
        let before = read_mxcsr();

        {
            let _guard = DenormalGuard::new();
            assert_eq!(
                read_mxcsr() & FTZ_DAZ,
                FTZ_DAZ,
                "guard must set FTZ and DAZ"
            );
            // MIN_POSITIVE * 0.5 is a denormal result; inside the guard it must be zero.
            let product = std::hint::black_box(f32::MIN_POSITIVE) * std::hint::black_box(0.5);
            assert_eq!(product, 0.0);
        }

        assert_eq!(read_mxcsr(), before, "drop must restore the previous MXCSR");
        if before & FTZ_DAZ == 0 {
            // Without the guard, the same product is a non-zero denormal.
            let product = std::hint::black_box(f32::MIN_POSITIVE) * std::hint::black_box(0.5);
            assert_ne!(product, 0.0);
        }
    }

    #[test]
    fn guard_can_be_created_and_dropped() {
        let guard = DenormalGuard::new();
        drop(guard);
    }
}
