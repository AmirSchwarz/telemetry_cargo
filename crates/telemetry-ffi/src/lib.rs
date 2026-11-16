//! Stable version-one C ABI. See `include/telemetry.h` for pointer ownership.

use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};
use telemetry_core::{Config, Telemetry};

pub const OK: i32 = 0;
pub const INVALID_INPUT: i32 = 1;
pub const INVALID_HANDLE: i32 = 2;
pub const SDK_ERROR: i32 = 3;
pub const PANIC: i32 = 4;
const MAX_INPUT_BYTES: usize = 1024 * 1024;

/// Owned bytes returned by this library. Not NUL terminated.
#[repr(C)]
#[derive(Debug)]
pub struct TelemetryBuffer {
    pub data: *mut u8,
    pub len: usize,
}

impl TelemetryBuffer {
    fn empty() -> Self {
        Self {
            data: ptr::null_mut(),
            len: 0,
        }
    }

    fn from_value(value: Value) -> Self {
        // Serializing a JSON value has no fallible user-controlled serializer.
        let bytes = value.to_string().into_bytes().into_boxed_slice();
        let len = bytes.len();
        Self {
            data: Box::into_raw(bytes).cast::<u8>(),
            len,
        }
    }
}

static HANDLES: OnceLock<Mutex<HashMap<u64, Telemetry>>> = OnceLock::new();
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

fn handles() -> &'static Mutex<HashMap<u64, Telemetry>> {
    HANDLES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_handle() -> Result<u64, i32> {
    // Never wrap: a stale handle must never refer to another SDK instance.
    let mut current = NEXT_HANDLE.load(Ordering::Relaxed);
    loop {
        let next = current.checked_add(1).ok_or(SDK_ERROR)?;
        match NEXT_HANDLE.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => return Ok(current),
            Err(observed) => current = observed,
        }
    }
}

unsafe fn input<'a>(data: *const u8, len: usize) -> Result<&'a str, i32> {
    if data.is_null() || len == 0 || len > MAX_INPUT_BYTES || len > isize::MAX as usize {
        return Err(INVALID_INPUT);
    }
    // SAFETY: the caller promises readable memory of `len` bytes.
    let bytes = unsafe { slice::from_raw_parts(data, len) };
    std::str::from_utf8(bytes).map_err(|_| INVALID_INPUT)
}

fn ffi_result(operation: impl FnOnce() -> Result<(), i32>) -> i32 {
    match catch_unwind(AssertUnwindSafe(operation)) {
        Ok(Ok(())) => OK,
        Ok(Err(code)) => code,
        Err(_) => PANIC,
    }
}

fn error_value(code: i32) -> Value {
    let message = match code {
        INVALID_INPUT => "invalid input",
        INVALID_HANDLE => "invalid handle",
        SDK_ERROR => "telemetry operation failed",
        _ => "internal panic",
    };
    json!({"error": {"code": code, "message": message}})
}

/// Initialize an SDK from UTF-8 JSON config.
///
/// # Safety
/// `out` must be aligned and writable for one `u64`; `config` must point to
/// `config_len` readable bytes for this call. All input bytes are copied.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn telemetry_v1_init(
    config: *const u8,
    config_len: usize,
    out: *mut u64,
) -> i32 {
    if out.is_null() {
        return INVALID_INPUT;
    }
    // SAFETY: guaranteed by the ABI caller.
    unsafe {
        out.write(0);
    }
    ffi_result(|| {
        let text = unsafe { input(config, config_len)? };
        let config: Config = serde_json::from_str(text).map_err(|_| INVALID_INPUT)?;
        let sdk = Telemetry::init(config).map_err(|_| SDK_ERROR)?;
        let handle = next_handle()?;
        handles().lock().map_err(|_| SDK_ERROR)?.insert(handle, sdk);
        unsafe {
            out.write(handle);
        }
        Ok(())
    })
}

/// Dispatch a JSON command. Successful and error responses are owned buffers.
///
/// # Safety
/// `out` must be aligned and writable for `TelemetryBuffer`. `request` must
/// reference `request_len` readable bytes. Do not alias `out` with the input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn telemetry_v1_command(
    handle: u64,
    request: *const u8,
    request_len: usize,
    out: *mut TelemetryBuffer,
) -> i32 {
    if out.is_null() {
        return INVALID_INPUT;
    }
    unsafe {
        out.write(TelemetryBuffer::empty());
    }
    let code = ffi_result(|| {
        let text = unsafe { input(request, request_len)? };
        let command: Value = serde_json::from_str(text).map_err(|_| INVALID_INPUT)?;
        let sdk = handles()
            .lock()
            .map_err(|_| SDK_ERROR)?
            .get(&handle)
            .cloned()
            .ok_or(INVALID_HANDLE)?;
        let response = sdk.command(command).map_err(|_| SDK_ERROR)?;
        unsafe {
            out.write(TelemetryBuffer::from_value(response));
        }
        Ok(())
    });
    if code != OK {
        // Contain response allocation/serialization panics as well.
        let response = catch_unwind(AssertUnwindSafe(|| {
            TelemetryBuffer::from_value(error_value(code))
        }));
        if let Ok(response) = response {
            unsafe {
                out.write(response);
            }
        }
    }
    code
}

/// Remove and shut down the SDK. Use a `shutdown` command first to flush.
/// Commands already holding a clone may finish; later handle lookups fail.
#[unsafe(no_mangle)]
pub extern "C" fn telemetry_v1_destroy(handle: u64) -> i32 {
    ffi_result(|| {
        let sdk = handles()
            .lock()
            .map_err(|_| SDK_ERROR)?
            .remove(&handle)
            .ok_or(INVALID_HANDLE)?;
        // Destruction always invalidates the handle, including failed final flush.
        let _ = sdk.shutdown(Duration::ZERO);
        Ok(())
    })
}

/// Release exactly one buffer returned by `telemetry_v1_command`.
///
/// # Safety
/// Pass the original data/len pair exactly once, to the library that created
/// it. Do not free through libc or reuse a copied buffer after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn telemetry_v1_buffer_free(buffer: TelemetryBuffer) {
    if buffer.data.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let raw = ptr::slice_from_raw_parts_mut(buffer.data, buffer.len);
        // SAFETY: caller guarantees this is an original live owned buffer.
        drop(unsafe { Box::from_raw(raw) });
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init() -> u64 {
        let config = br#"{"app_id":"ffi.test","app_version":"1.0"}"#;
        let mut handle = 0;
        assert_eq!(
            unsafe { telemetry_v1_init(config.as_ptr(), config.len(), &mut handle) },
            OK
        );
        assert_ne!(handle, 0);
        handle
    }

    fn command(handle: u64, text: &[u8]) -> (i32, Value) {
        let mut output = TelemetryBuffer::empty();
        let status =
            unsafe { telemetry_v1_command(handle, text.as_ptr(), text.len(), &mut output) };
        assert!(!output.data.is_null());
        let result =
            serde_json::from_slice(unsafe { slice::from_raw_parts(output.data, output.len) })
                .unwrap();
        unsafe {
            telemetry_v1_buffer_free(output);
        }
        (status, result)
    }

    #[test]
    fn initialization_rejects_null_invalid_utf8_and_resets_output() {
        let mut handle = 77;
        assert_eq!(
            unsafe { telemetry_v1_init(ptr::null(), 1, &mut handle) },
            INVALID_INPUT
        );
        assert_eq!(handle, 0);
        assert_eq!(
            unsafe { telemetry_v1_init([255].as_ptr(), 1, &mut handle) },
            INVALID_INPUT
        );
        assert_eq!(
            unsafe { telemetry_v1_init(b"{}".as_ptr(), 2, ptr::null_mut()) },
            INVALID_INPUT
        );
    }

    #[test]
    fn owned_responses_and_stale_handles() {
        let handle = init();
        assert_eq!(command(handle, br#"{"op":"diagnostics"}"#).0, OK);
        assert_eq!(command(handle, &[255]).0, INVALID_INPUT);
        assert_eq!(
            command(handle, br#"{"op":"unknown","secret":"password"}"#),
            (SDK_ERROR, error_value(SDK_ERROR))
        );
        assert_eq!(telemetry_v1_destroy(handle), OK);
        assert_eq!(telemetry_v1_destroy(handle), INVALID_HANDLE);
        assert_eq!(
            command(handle, br#"{"op":"diagnostics"}"#).0,
            INVALID_HANDLE
        );
        let second = init();
        assert_ne!(handle, second);
        assert_eq!(telemetry_v1_destroy(second), OK);
    }

    #[test]
    fn commands_are_thread_safe() {
        let handle = init();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        assert_eq!(command(handle, br#"{"op":"diagnostics"}"#).0, OK);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(telemetry_v1_destroy(handle), OK);
    }

    #[test]
    fn panic_is_contained() {
        assert_eq!(ffi_result(|| panic!("synthetic panic")), PANIC);
    }
}
