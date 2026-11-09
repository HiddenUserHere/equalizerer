//! "Start with Windows": a value in the current user's `Run` key that starts the app at sign-in.
//!
//! Only `HKEY_CURRENT_USER` is touched, so no administrator rights are needed and other users are
//! not affected. The value holds the command line as written; the caller builds it, including the
//! quotes around the program path.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ, RegCloseKey,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::core::HSTRING;

use crate::error::{ComResultExt, WasapiError};

/// Path of the per-user autostart list, relative to `HKEY_CURRENT_USER`.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Owns an opened registry key and closes it exactly once, on drop.
struct RunKey(HKEY);

impl Drop for RunKey {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by a successful `RegOpenKeyExW` and is closed only
        // here. A failing close leaves nothing to recover, so the result is ignored.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

/// Opens the per-user `Run` key with the given access rights.
fn open_run_key(access: REG_SAM_FLAGS) -> Result<RunKey, WasapiError> {
    let subkey = HSTRING::from(RUN_KEY);
    let mut key = HKEY::default();
    // SAFETY: `subkey` outlives the call, and `key` is a valid out pointer that receives the
    // handle only on success. The handle is wrapped in `RunKey` right after the call.
    unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &subkey, None, access, &raw mut key) }
        .ok()
        .context("RegOpenKeyExW(Run)")?;
    Ok(RunKey(key))
}

/// Tells whether a value called `app_name` exists in the per-user `Run` key.
///
/// # Errors
///
/// Returns [`WasapiError::Com`] when the key cannot be opened or queried.
pub fn is_enabled(app_name: &str) -> Result<bool, WasapiError> {
    let key = open_run_key(KEY_QUERY_VALUE)?;
    let name = HSTRING::from(app_name);
    // SAFETY: `key` is open with query access and `name` outlives the call. Null data and size
    // pointers ask only whether the value exists, so no buffer is written.
    let status = unsafe { RegQueryValueExW(key.0, &name, None, None, None, None) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    status.ok().context("RegQueryValueExW")?;
    Ok(true)
}

/// Creates or replaces the value `app_name` so that Windows runs `command` at sign-in.
///
/// # Errors
///
/// Returns [`WasapiError::Com`] when the key cannot be opened or the value cannot be written.
pub fn enable(app_name: &str, command: &str) -> Result<(), WasapiError> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let name = HSTRING::from(app_name);
    // REG_SZ holds UTF-16 code units followed by a terminating NUL, in little-endian order.
    let data: Vec<u8> = command
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: `key` is open with set-value access, `name` outlives the call, and `data` is a
    // byte slice whose length is passed by the binding; the API only reads it.
    unsafe { RegSetValueExW(key.0, &name, None, REG_SZ, Some(&data)) }
        .ok()
        .context("RegSetValueExW")
}

/// Removes the value `app_name` from the per-user `Run` key. A missing value is not an error.
///
/// # Errors
///
/// Returns [`WasapiError::Com`] when the key cannot be opened or the value cannot be deleted.
pub fn disable(app_name: &str) -> Result<(), WasapiError> {
    let key = open_run_key(KEY_SET_VALUE)?;
    let name = HSTRING::from(app_name);
    // SAFETY: `key` is open with set-value access and `name` outlives the call.
    let status = unsafe { RegDeleteValueW(key.0, &name) };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_SUCCESS {
        return Ok(());
    }
    status.ok().context("RegDeleteValueW")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Removes the test value when the test ends, also when an assertion fails.
    struct RemoveOnDrop<'a>(&'a str);

    impl Drop for RemoveOnDrop<'_> {
        fn drop(&mut self) {
            let _ = disable(self.0);
        }
    }

    #[test]
    fn enable_disable_round_trip() {
        // A unique name keeps the test away from a real "Equalizerer" entry.
        let name = format!("EqualizererTest-{}", std::process::id());
        let _cleanup = RemoveOnDrop(&name);

        assert!(!is_enabled(&name).expect("query before enable"));
        enable(&name, r#""C:\test\eq-app.exe" --minimized"#).expect("enable");
        assert!(is_enabled(&name).expect("query after enable"));
        disable(&name).expect("disable");
        assert!(!is_enabled(&name).expect("query after disable"));
        // Deleting a value that is already gone must succeed.
        disable(&name).expect("second disable");
    }
}
