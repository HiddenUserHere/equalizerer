//! COM apartment RAII guard and string helpers.

use std::ffi::c_void;
use std::marker::PhantomData;

use windows::Win32::System::Com::{
    COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::core::PWSTR;

use crate::error::{ComResultExt, WasapiError};

/// A COM apartment joined by the current thread (multithreaded, MTA).
///
/// Creating the guard calls `CoInitializeEx`; dropping it calls `CoUninitialize`. Both `S_OK`
/// and `S_FALSE` (already initialized in the same mode) count as success and are balanced by a
/// `CoUninitialize` on drop. `RPC_E_CHANGED_MODE` (the thread already uses a single-threaded
/// apartment) is an error.
///
/// The guard is neither `Send` nor `Sync`: COM requires uninitialize on the thread that
/// initialized.
#[derive(Debug)]
pub struct ComApartment {
    _thread_bound: PhantomData<*mut ()>,
}

impl ComApartment {
    /// Initializes COM on the current thread in the multithreaded apartment.
    ///
    /// # Errors
    ///
    /// Returns [`WasapiError::Com`] when `CoInitializeEx` fails, including
    /// `RPC_E_CHANGED_MODE` when the thread already uses a single-threaded apartment.
    pub fn init_mta() -> Result<Self, WasapiError> {
        // SAFETY: `pvreserved` is null, as the API requires. The call only changes the COM state
        // of the calling thread, and the result is checked below.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        // S_OK and S_FALSE both succeed and both must be balanced by CoUninitialize, which
        // `Drop` does for every constructed guard.
        hr.ok().context("CoInitializeEx(MTA)")?;
        Ok(Self {
            _thread_bound: PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        // SAFETY: this guard was only constructed after a successful `CoInitializeEx` on this
        // thread, and the guard is `!Send`, so this is the same thread. Each guard holds one
        // initialization count, so one `CoUninitialize` per guard is balanced.
        unsafe { CoUninitialize() };
    }
}

/// Copies a COM-allocated UTF-16 string into a `String` and frees the original.
///
/// Invalid UTF-16 is replaced with U+FFFD. A null pointer yields an empty string.
///
/// # Safety
///
/// `pwstr` must be null, or point to a NUL-terminated UTF-16 string allocated with the COM task
/// allocator (for example, the result of `IMMDevice::GetId`). Ownership transfers to this
/// function, which frees the memory exactly once. The caller must not use `pwstr` afterwards.
#[must_use]
pub unsafe fn pwstr_to_string(pwstr: PWSTR) -> String {
    if pwstr.is_null() {
        return String::new();
    }
    // SAFETY: the caller guarantees a valid NUL-terminated string, so `as_wide` reads only
    // initialized memory up to the terminator.
    let text = String::from_utf16_lossy(unsafe { pwstr.as_wide() });
    // SAFETY: the caller guarantees the memory came from the COM task allocator and is freed
    // only here.
    unsafe { CoTaskMemFree(Some(pwstr.as_ptr().cast::<c_void>().cast_const())) };
    text
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Com::CoTaskMemAlloc;
    use windows::core::PWSTR;

    use super::*;

    #[test]
    fn init_and_drop_twice_on_one_thread() {
        let first = ComApartment::init_mta().expect("first CoInitializeEx");
        drop(first);
        let second = ComApartment::init_mta().expect("second CoInitializeEx");
        drop(second);
    }

    #[test]
    fn nested_apartments_are_balanced() {
        // The second init returns S_FALSE; both guards must uninitialize.
        let outer = ComApartment::init_mta().expect("outer CoInitializeEx");
        let inner = ComApartment::init_mta().expect("inner CoInitializeEx");
        drop(inner);
        drop(outer);
    }

    #[test]
    fn pwstr_to_string_copies_and_frees() {
        let units: Vec<u16> = "Speakers \u{00e9}".encode_utf16().chain([0]).collect();
        let bytes = units.len() * std::mem::size_of::<u16>();
        // SAFETY: CoTaskMemAlloc returns memory from the COM task allocator, or null on failure.
        let raw = unsafe { CoTaskMemAlloc(bytes) }.cast::<u16>();
        assert!(!raw.is_null(), "CoTaskMemAlloc failed");
        // SAFETY: `raw` is a fresh allocation of `units.len()` u16 values, and the source has
        // exactly that many elements; the regions do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), raw, units.len()) };
        // SAFETY: `raw` is a COM-allocated, NUL-terminated string that is not used afterwards.
        let text = unsafe { pwstr_to_string(PWSTR::from_raw(raw)) };
        assert_eq!(text, "Speakers \u{00e9}");
    }

    #[test]
    fn pwstr_to_string_accepts_null() {
        // SAFETY: a null pointer is explicitly allowed by the contract.
        let text = unsafe { pwstr_to_string(PWSTR::null()) };
        assert_eq!(text, "");
    }
}
