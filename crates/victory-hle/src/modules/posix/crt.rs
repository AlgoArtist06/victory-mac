//! The Universal C runtime (`api-ms-win-crt-*`): start-up, exit, errno and
//! the heap. Windows `long` is 32 bits and `wchar_t` 16, so functions that
//! take them are ours, never libc's under the same name.
#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};
use std::sync::Mutex;

use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(_initialize_onexit_table),
        hle_export!(_register_onexit_function),
        hle_export!(_execute_onexit_table),
        hle_export!(_crt_atexit),
        hle_export!(_crt_at_quick_exit),
        hle_export!(_initterm),
        hle_export!(_initterm_e),
        hle_export!(_configure_narrow_argv),
        hle_export!(_initialize_narrow_environment),
        hle_export!(_get_narrow_winmain_command_line),
        hle_export!(_set_app_type),
        hle_export!(_seh_filter_exe),
        hle_export!(_seh_filter_dll),
        hle_export!(_register_thread_local_exe_atexit_callback),
        hle_export!(_cexit),
        hle_export!(_c_exit),
        hle_export!(_exit),
        hle_export!(_errno),
        hle_export!(_get_errno),
        hle_export!(_set_errno),
        hle_export!(_invalid_parameter_noinfo),
        hle_export!(_invoke_watson),
        hle_export!(_set_invalid_parameter_handler),
        hle_export!(_set_new_handler),
        hle_export!(_set_new_mode),
        hle_export!(_callnewh),
        hle_export!(_wassert),
        hle_export!(malloc),
        hle_export!(calloc),
        hle_export!(realloc),
        hle_export!(free),
    ]
}

type Void = unsafe extern "win64" fn();
type Int = unsafe extern "win64" fn() -> i32;

/// `_onexit_table_t`: first, last and end of an array of functions.
#[repr(C)]
struct OnExitTable {
    first: *mut Void,
    last: *mut Void,
    end: *mut Void,
}

extern "win64" fn _initialize_onexit_table(table: *mut OnExitTable) -> i32 {
    if table.is_null() {
        return -1;
    }
    // SAFETY: the caller's table.
    unsafe {
        table.write(OnExitTable {
            first: std::ptr::null_mut(),
            last: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
        })
    };
    0
}

extern "win64" fn _register_onexit_function(table: *mut OnExitTable, f: Void) -> i32 {
    // SAFETY: an initialised table whose array is ours (libc's heap).
    unsafe {
        let t = &mut *table;
        if t.last == t.end {
            let len = t.last.offset_from(t.first).max(0) as usize;
            let cap = (len * 2).max(8);
            let grown = libc::realloc(t.first.cast(), cap * size_of::<Void>()).cast::<Void>();
            if grown.is_null() {
                return -1;
            }
            t.first = grown;
            t.last = grown.add(len);
            t.end = grown.add(cap);
        }
        t.last.write(f);
        t.last = t.last.add(1);
    }
    0
}

/// Calls the table's functions, last registered first, and empties it.
extern "win64" fn _execute_onexit_table(table: *mut OnExitTable) -> i32 {
    // SAFETY: an initialised table.
    let t = unsafe { std::ptr::replace(table, OnExitTable {
        first: std::ptr::null_mut(),
        last: std::ptr::null_mut(),
        end: std::ptr::null_mut(),
    }) };
    let mut at = t.last;
    while at > t.first {
        // SAFETY: inside the array; each entry a registered function.
        unsafe {
            at = at.sub(1);
            (at.read())();
        }
    }
    // SAFETY: the array came from libc's heap.
    unsafe { libc::free(t.first.cast()) };
    0
}

/// The process's own `atexit` table (`_crt_atexit`), run by `_cexit`.
static AT_EXIT: Mutex<Vec<usize>> = Mutex::new(Vec::new());
static AT_QUICK_EXIT: Mutex<Vec<usize>> = Mutex::new(Vec::new());

extern "win64" fn _crt_atexit(f: Void) -> i32 {
    AT_EXIT.lock().unwrap_or_else(|e| e.into_inner()).push(f as usize);
    0
}

extern "win64" fn _crt_at_quick_exit(f: Void) -> i32 {
    AT_QUICK_EXIT.lock().unwrap_or_else(|e| e.into_inner()).push(f as usize);
    0
}

extern "win64" fn _initterm(first: *const Option<Void>, last: *const Option<Void>) {
    let mut at = first;
    while at < last {
        // SAFETY: the image's own initialiser array.
        unsafe {
            if let Some(f) = at.read() {
                f();
            }
            at = at.add(1);
        }
    }
}

extern "win64" fn _initterm_e(first: *const Option<Int>, last: *const Option<Int>) -> i32 {
    let mut at = first;
    while at < last {
        // SAFETY: as above.
        unsafe {
            if let Some(f) = at.read() {
                let code = f();
                if code != 0 {
                    return code;
                }
            }
            at = at.add(1);
        }
    }
    0
}

extern "win64" fn _configure_narrow_argv(_mode: i32) -> i32 {
    0
}

extern "win64" fn _initialize_narrow_environment() -> i32 {
    0
}

extern "win64" fn _get_narrow_winmain_command_line() -> *const c_char {
    c"".as_ptr()
}

extern "win64" fn _set_app_type(_: i32) {}

const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

extern "win64" fn _seh_filter_exe(_code: u32, _pointers: *mut c_void) -> i32 {
    EXCEPTION_CONTINUE_SEARCH
}

extern "win64" fn _seh_filter_dll(_code: u32, _pointers: *mut c_void) -> i32 {
    EXCEPTION_CONTINUE_SEARCH
}

/// ponytail: thread-local destructors of the executable are not called at
/// thread end.
extern "win64" fn _register_thread_local_exe_atexit_callback(_: *const c_void) {}

extern "win64" fn _cexit() {
    let fns = std::mem::take(&mut *AT_EXIT.lock().unwrap_or_else(|e| e.into_inner()));
    for f in fns.into_iter().rev() {
        // SAFETY: registered by the title.
        unsafe { std::mem::transmute::<usize, Void>(f)() };
    }
}

extern "win64" fn _c_exit() {}

extern "win64" fn _exit(code: i32) -> ! {
    // SAFETY: ends the process.
    unsafe { libc::_exit(code) }
}

thread_local! {
    /// The C runtime's `errno`, in Windows numbering (the small values
    /// match POSIX).
    static ERRNO: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

extern "win64" fn _errno() -> *mut i32 {
    ERRNO.with(|e| e.as_ptr())
}

pub(crate) fn set_errno(value: i32) {
    ERRNO.set(value);
}

extern "win64" fn _get_errno(out: *mut i32) -> i32 {
    // SAFETY: the caller's out pointer.
    unsafe { out.write(ERRNO.get()) };
    0
}

extern "win64" fn _set_errno(value: i32) -> i32 {
    ERRNO.set(value);
    0
}

extern "win64" fn _invalid_parameter_noinfo() {
    tracing::warn!("the C runtime was given an invalid parameter");
}

extern "win64" fn _invoke_watson(
    _: *const u16,
    _: *const u16,
    _: *const u16,
    _: u32,
    _: usize,
) -> ! {
    tracing::error!("the C runtime asked to end the process (_invoke_watson)");
    std::process::abort()
}

static INVALID_PARAMETER_HANDLER: Mutex<usize> = Mutex::new(0);
static NEW_HANDLER: Mutex<usize> = Mutex::new(0);

extern "win64" fn _set_invalid_parameter_handler(handler: usize) -> usize {
    std::mem::replace(&mut INVALID_PARAMETER_HANDLER.lock().unwrap_or_else(|e| e.into_inner()), handler)
}

extern "win64" fn _set_new_handler(handler: usize) -> usize {
    std::mem::replace(&mut NEW_HANDLER.lock().unwrap_or_else(|e| e.into_inner()), handler)
}

extern "win64" fn _set_new_mode(_: i32) -> i32 {
    0
}

/// Whether a new handler freed memory to retry an allocation: none is
/// called, so no.
extern "win64" fn _callnewh(_size: usize) -> i32 {
    0
}

extern "win64" fn _wassert(message: *const u16, file: *const u16, line: u32) -> ! {
    let text = |p: *const u16| {
        if p.is_null() {
            return String::new();
        }
        // SAFETY: NUL-terminated wide strings from the title.
        let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
    };
    tracing::error!(message = text(message), file = text(file), line, "the title's assertion failed");
    std::process::abort()
}

extern "win64" fn malloc(size: usize) -> *mut c_void {
    // SAFETY: plain allocation (16-byte aligned, as Windows gives).
    unsafe { libc::malloc(size) }
}

extern "win64" fn calloc(count: usize, size: usize) -> *mut c_void {
    // SAFETY: as above.
    unsafe { libc::calloc(count, size) }
}

extern "win64" fn realloc(p: *mut c_void, size: usize) -> *mut c_void {
    // SAFETY: a block from these functions, or null.
    unsafe { libc::realloc(p, size) }
}

extern "win64" fn free(p: *mut c_void) {
    // SAFETY: as above.
    unsafe { libc::free(p) }
}
