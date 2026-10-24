//! C ABI surface — everything declared in include/logkit.h.
//! Conventions (house standard):
//! - Failures set a thread-local last-error (code + message) and
//!   return the documented sentinel.
//! - Nothing unwinds across the FFI boundary (catch_unwind
//!   everywhere; release profile also sets panic = "abort").
//! - A logger handle IS thread-safe: writes are serialized
//!   internally, so any thread may log through it — the normal
//!   case for a logger.

// The exported functions ARE the raw-pointer C ABI declared in
// include/logkit.h; pointer validity is the documented contract.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};
use std::panic::catch_unwind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

use crate::platform::FileSink;
use crate::{LkResult, ERR_INTERNAL, ERR_INVALID_ARG, LK_OK};

/// Open flag: sync every record to disk (sync_data) after writing.
/// Slower; for logs that must survive an OS crash, not just an
/// app crash. Off by default — flush-per-record already covers
/// process crashes.
pub const SYNC_ON_WRITE: u32 = 0x1;

/// The opaque logger handle (LkLogger in the header).
pub struct LkLogger {
    sink: Mutex<FileSink>,
    min_level: AtomicI32,
}

thread_local! {
    static LAST_ERROR: RefCell<(i32, CString)> =
        RefCell::new((0, CString::new("").unwrap()));
}

pub fn set_error(code: i32, msg: &str) {
    let clean: String = msg.chars().filter(|&c| c != '\0').collect();
    LAST_ERROR.with(|e| {
        *e.borrow_mut() = (
            code,
            CString::new(clean).unwrap_or_else(|_| CString::new("error").unwrap()),
        );
    });
}

fn clear_error() {
    set_error(LK_OK, "");
}

fn fail(code: i32, msg: &str) -> c_int {
    set_error(code, msg);
    code as c_int
}

fn panic_code() -> c_int {
    set_error(ERR_INTERNAL, "internal panic caught at the FFI boundary");
    ERR_INTERNAL as c_int
}

fn guard<T, F: FnOnce() -> T>(f: F) -> std::thread::Result<T> {
    catch_unwind(std::panic::AssertUnwindSafe(f))
}

fn code_of<T>(inner: LkResult<T>) -> c_int {
    match inner {
        Ok(_) => {
            clear_error();
            LK_OK as c_int
        }
        Err((code, msg)) => fail(code, &msg),
    }
}

fn cstr<'a>(ptr: *const c_char, what: &str) -> LkResult<&'a str> {
    if ptr.is_null() {
        return Err((ERR_INVALID_ARG, format!("{what} must not be null")));
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| (ERR_INVALID_ARG, format!("{what} must be valid UTF-8")))
}

fn logger<'a>(ptr: *mut LkLogger) -> LkResult<&'a LkLogger> {
    if ptr.is_null() {
        return Err((
            ERR_INVALID_ARG,
            "logger handle must not be null".to_string(),
        ));
    }
    Ok(unsafe { &*ptr })
}

#[no_mangle]
pub extern "C" fn lk_version() -> *const c_char {
    static VERSION_C: &[u8] = b"0.1.0\0";
    VERSION_C.as_ptr() as *const c_char
}

#[no_mangle]
pub extern "C" fn lk_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().1.as_ptr())
}

#[no_mangle]
pub extern "C" fn lk_last_error_code() -> c_int {
    LAST_ERROR.with(|e| e.borrow().0) as c_int
}

/// # Safety (caller contract): `p` is null or a string this library
/// returned and has not been freed yet. Documented in the header.
#[no_mangle]
pub extern "C" fn lk_free_string(p: *mut c_char) {
    if !p.is_null() {
        unsafe {
            drop(CString::from_raw(p));
        }
    }
}

fn now_millis() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn lk_open(
    path: *const c_char,
    min_level: c_int,
    max_bytes: u64,
    keep_files: u32,
    flags: u32,
    out: *mut *mut LkLogger,
) -> c_int {
    match guard(|| {
        if out.is_null() {
            return fail(ERR_INVALID_ARG, "out must not be null");
        }
        unsafe { *out = std::ptr::null_mut() };
        let inner = (|| -> LkResult<LkLogger> {
            let p = cstr(path, "path")?;
            if !crate::valid_level(min_level) {
                return Err((
                    ERR_INVALID_ARG,
                    format!("invalid min_level {min_level} (expected 1..=5)"),
                ));
            }
            let sink = FileSink::open(
                &PathBuf::from(p),
                max_bytes,
                keep_files,
                flags & SYNC_ON_WRITE != 0,
            )?;
            Ok(LkLogger {
                sink: Mutex::new(sink),
                min_level: AtomicI32::new(min_level),
            })
        })();
        match inner {
            Ok(lg) => {
                unsafe { *out = Box::into_raw(Box::new(lg)) };
                clear_error();
                LK_OK as c_int
            }
            Err((c, m)) => fail(c, &m),
        }
    }) {
        Ok(code) => code,
        Err(_) => panic_code(),
    }
}

/// Flush and free the logger. Logging through the handle after
/// this is a contract violation (use-after-close).
#[no_mangle]
pub extern "C" fn lk_close(lg: *mut LkLogger) {
    if !lg.is_null() {
        let _ = guard(|| unsafe {
            let boxed = Box::from_raw(lg);
            if let Ok(mut sink) = boxed.sink.lock() {
                let _ = sink.flush();
            }
            drop(boxed);
        });
    }
}

fn log_impl(
    lg: *mut LkLogger,
    level: c_int,
    target: *const c_char,
    message: *const c_char,
    fields_json: *const c_char,
) -> c_int {
    let inner = (|| -> LkResult<()> {
        let l = logger(lg)?;
        if !crate::valid_level(level) {
            return Err((
                ERR_INVALID_ARG,
                format!("invalid level {level} (expected 1..=5)"),
            ));
        }
        if level < l.min_level.load(Ordering::Relaxed) {
            return Ok(()); // filtered: not an error, nothing written
        }
        let target = cstr(target, "target")?;
        let message = cstr(message, "message")?;
        let fields = if fields_json.is_null() {
            Vec::new()
        } else {
            crate::fields::parse_fields(cstr(fields_json, "fields_json")?)?
        };
        let line = crate::format_record(now_millis(), level, target, message, &fields)?;
        let mut sink = l
            .sink
            .lock()
            .map_err(|_| (ERR_INTERNAL, "logger mutex poisoned".to_string()))?;
        sink.write_record(&line)
    })();
    code_of(inner)
}

#[no_mangle]
pub extern "C" fn lk_log(
    lg: *mut LkLogger,
    level: c_int,
    target: *const c_char,
    message: *const c_char,
) -> c_int {
    match guard(|| log_impl(lg, level, target, message, std::ptr::null())) {
        Ok(code) => code,
        Err(_) => panic_code(),
    }
}

#[no_mangle]
pub extern "C" fn lk_log_fields(
    lg: *mut LkLogger,
    level: c_int,
    target: *const c_char,
    message: *const c_char,
    fields_json: *const c_char,
) -> c_int {
    match guard(|| log_impl(lg, level, target, message, fields_json)) {
        Ok(code) => code,
        Err(_) => panic_code(),
    }
}

#[no_mangle]
pub extern "C" fn lk_flush(lg: *mut LkLogger) -> c_int {
    match guard(|| {
        let inner = (|| -> LkResult<()> {
            let l = logger(lg)?;
            let mut sink = l
                .sink
                .lock()
                .map_err(|_| (ERR_INTERNAL, "logger mutex poisoned".to_string()))?;
            sink.flush()
        })();
        code_of(inner)
    }) {
        Ok(code) => code,
        Err(_) => panic_code(),
    }
}

#[no_mangle]
pub extern "C" fn lk_set_level(lg: *mut LkLogger, level: c_int) -> c_int {
    match guard(|| {
        let inner = (|| -> LkResult<()> {
            let l = logger(lg)?;
            if !crate::valid_level(level) {
                return Err((
                    ERR_INVALID_ARG,
                    format!("invalid level {level} (expected 1..=5)"),
                ));
            }
            l.min_level.store(level, Ordering::Relaxed);
            Ok(())
        })();
        code_of(inner)
    }) {
        Ok(code) => code,
        Err(_) => panic_code(),
    }
}

/// 1 = a record at `level` would be written, 0 = filtered,
/// -1 = error.
#[no_mangle]
pub extern "C" fn lk_enabled(lg: *mut LkLogger, level: c_int) -> c_int {
    match guard(|| match logger(lg) {
        Ok(l) => {
            if !crate::valid_level(level) {
                set_error(ERR_INVALID_ARG, "invalid level (expected 1..=5)");
                -1
            } else {
                clear_error();
                if level >= l.min_level.load(Ordering::Relaxed) {
                    1
                } else {
                    0
                }
            }
        }
        Err((c, m)) => {
            set_error(c, &m);
            -1
        }
    }) {
        Ok(code) => code,
        Err(_) => panic_code(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn cs(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "logkit-ffi-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn ffi_end_to_end() {
        let dir = temp_dir("e2e");
        let path = dir.join("app.log");
        let path_c = cs(path.to_str().unwrap());
        let mut lg: *mut LkLogger = std::ptr::null_mut();
        assert_eq!(
            lk_open(path_c.as_ptr(), crate::LEVEL_INFO, 0, 3, 0, &mut lg),
            LK_OK as c_int
        );
        assert!(!lg.is_null());

        let target = cs("app");
        let msg = cs("started");
        assert_eq!(
            lk_log(lg, crate::LEVEL_INFO, target.as_ptr(), msg.as_ptr()),
            LK_OK as c_int
        );
        // DEBUG is below the INFO floor: accepted, not written.
        let dbg = cs("details");
        assert_eq!(
            lk_log(lg, crate::LEVEL_DEBUG, target.as_ptr(), dbg.as_ptr()),
            LK_OK as c_int
        );
        assert_eq!(lk_enabled(lg, crate::LEVEL_DEBUG), 0);
        assert_eq!(lk_enabled(lg, crate::LEVEL_ERROR), 1);

        // Structured fields.
        let net = cs("net");
        let conn = cs("connected");
        let fields = cs(r#"{"host":"example.com","attempt":2}"#);
        assert_eq!(
            lk_log_fields(
                lg,
                crate::LEVEL_WARN,
                net.as_ptr(),
                conn.as_ptr(),
                fields.as_ptr()
            ),
            LK_OK as c_int
        );
        // Bad fields JSON: error, nothing written.
        let bad = cs(r#"{"a":{"nested":1}}"#);
        assert_eq!(
            lk_log_fields(
                lg,
                crate::LEVEL_WARN,
                net.as_ptr(),
                conn.as_ptr(),
                bad.as_ptr()
            ),
            crate::ERR_PARSE as c_int
        );
        lk_close(lg);

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].ends_with(" INFO  app: started"), "{}", lines[0]);
        assert!(
            lines[1].ends_with(" WARN  net: connected host=example.com attempt=2"),
            "{}",
            lines[1]
        );
        // Timestamp shape: fixed-width ISO prefix.
        assert_eq!(
            lines[0].len() - " INFO  app: started".len(),
            24,
            "{}",
            lines[0]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
