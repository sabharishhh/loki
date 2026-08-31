//! The bridge.
//!
//! A small C ABI plus a registered callback the core invokes with serialized events and token
//! chunks. Roughly 250 lines when complete. UniFFI is heavier than this surface needs.
//!
//! Planned surface, from `.agent/ARCHITECTURE.md`:
//! `send_message`, `interrupt`, `confirm_action`, `undo_action`, `resolve_conflict`,
//! `set_grant`, `connect`, `disconnect`, plus the event callback.
//!
//! Only the version probe exists so far. It proves the whole chain links: Rust compiles to a
//! static library, Swift links it, and the app can call across.

use std::ffi::{CString, c_char};

/// Returns the core version as a NUL-terminated C string.
///
/// # Safety
/// The caller owns the returned pointer and must release it with [`loki_string_free`].
/// Returns null only if the version string somehow contains an interior NUL, which cannot
/// happen for a Cargo version.
#[unsafe(no_mangle)]
pub extern "C" fn loki_version() -> *mut c_char {
    match CString::new(loki_core::VERSION) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Releases a string returned by this library.
///
/// # Safety
/// `ptr` must be null, or a pointer returned by a function in this library and not yet freed.
/// Passing any other pointer, or freeing twice, is undefined behaviour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn loki_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: the caller contract above guarantees this came from `CString::into_raw`.
    drop(unsafe { CString::from_raw(ptr) });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_round_trips_and_frees() {
        let ptr = loki_version();
        assert!(!ptr.is_null());
        // SAFETY: `ptr` came from `loki_version`, which returns a valid CString pointer.
        let s = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().unwrap();
        assert_eq!(s, loki_core::VERSION);
        // SAFETY: `ptr` is the pointer we just received and have not freed.
        unsafe { loki_string_free(ptr) };
    }

    #[test]
    fn free_tolerates_null() {
        // SAFETY: null is explicitly allowed by the contract.
        unsafe { loki_string_free(std::ptr::null_mut()) };
    }
}
