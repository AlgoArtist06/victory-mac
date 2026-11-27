//! The C runtime's time functions (`api-ms-win-crt-time`). Windows'
//! `struct tm` is the bare C one; macOS's adds a UTC offset and a zone
//! name, so each call converts.
#![allow(non_snake_case)]

use std::ffi::{CString, c_char, c_void};

use super::crt::set_errno;
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(_time64),
        hle_export!(_difftime64),
        hle_export!(_ftime64),
        hle_export!(_gmtime64),
        hle_export!(_gmtime64_s),
        hle_export!(_localtime64_s),
        hle_export!(_mktime64),
        hle_export!(_mkgmtime64),
        hle_export!(strftime),
        hle_export!(_Strftime),
        hle_export!(_Wcsftime),
        hle_export!(_Getdays),
        hle_export!(_Getmonths),
        hle_export!(_Gettnames),
        hle_export!(_W_Getdays),
        hle_export!(_W_Getmonths),
        hle_export!(_W_Gettnames),
    ]
}

/// Windows' `struct tm`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Tm([i32; 9]);

fn to_libc(t: &Tm) -> libc::tm {
    let [sec, min, hour, mday, mon, year, wday, yday, isdst] = t.0;
    // SAFETY: plain data; zero is valid.
    let mut out: libc::tm = unsafe { std::mem::zeroed() };
    (out.tm_sec, out.tm_min, out.tm_hour, out.tm_mday, out.tm_mon, out.tm_year, out.tm_wday, out.tm_yday, out.tm_isdst) =
        (sec, min, hour, mday, mon, year, wday, yday, isdst);
    out
}

fn from_libc(t: &libc::tm) -> Tm {
    Tm([t.tm_sec, t.tm_min, t.tm_hour, t.tm_mday, t.tm_mon, t.tm_year, t.tm_wday, t.tm_yday, t.tm_isdst])
}

extern "win64" fn _time64(out: *mut i64) -> i64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { out.write(now) };
    }
    now
}

extern "win64" fn _difftime64(a: i64, b: i64) -> f64 {
    (a - b) as f64
}

/// `struct __timeb64`: seconds, milliseconds, minutes west of UTC, DST.
#[repr(C)]
struct TimeB {
    time: i64,
    millis: u16,
    timezone: i16,
    dst: i16,
}

extern "win64" fn _ftime64(out: *mut TimeB) {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = since.as_secs() as i64;
    // SAFETY: plain conversion into our struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&seconds, &mut tm) };
    // SAFETY: the caller's struct.
    unsafe {
        out.write(TimeB {
            time: seconds,
            millis: since.subsec_millis() as u16,
            timezone: (-(tm.tm_gmtoff / 60) + if tm.tm_isdst > 0 { 60 } else { 0 }) as i16,
            dst: tm.tm_isdst.max(0) as i16,
        })
    };
}

fn convert(time: *const i64, local: bool) -> Option<Tm> {
    if time.is_null() {
        return None;
    }
    // SAFETY: the caller's time and our struct.
    let seconds = unsafe { time.read() };
    if seconds < 0 {
        return None;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let done = unsafe {
        if local {
            libc::localtime_r(&seconds, &mut tm)
        } else {
            libc::gmtime_r(&seconds, &mut tm)
        }
    };
    (!done.is_null()).then(|| from_libc(&tm))
}

thread_local! {
    static GMTIME: std::cell::Cell<Tm> = const { std::cell::Cell::new(Tm([0; 9])) };
}

extern "win64" fn _gmtime64(time: *const i64) -> *mut Tm {
    match convert(time, false) {
        Some(tm) => {
            GMTIME.set(tm);
            GMTIME.with(std::cell::Cell::as_ptr)
        }
        None => {
            set_errno(22);
            std::ptr::null_mut()
        }
    }
}

fn convert_s(out: *mut Tm, time: *const i64, local: bool) -> i32 {
    if out.is_null() {
        return 22;
    }
    match convert(time, local) {
        Some(tm) => {
            // SAFETY: the caller's struct.
            unsafe { out.write(tm) };
            0
        }
        None => {
            // SAFETY: as above.
            unsafe { out.write(Tm([-1; 9])) };
            22
        }
    }
}

extern "win64" fn _gmtime64_s(out: *mut Tm, time: *const i64) -> i32 {
    convert_s(out, time, false)
}

extern "win64" fn _localtime64_s(out: *mut Tm, time: *const i64) -> i32 {
    convert_s(out, time, true)
}

/// `mktime`/`timegm`, writing the normalised fields back.
fn make(t: *mut Tm, f: unsafe extern "C" fn(*mut libc::tm) -> libc::time_t) -> i64 {
    // SAFETY: the caller's struct.
    let mut tm = to_libc(unsafe { &*t });
    // SAFETY: our struct.
    let seconds = unsafe { f(&mut tm) };
    if seconds != -1 {
        // SAFETY: as above.
        unsafe { t.write(from_libc(&tm)) };
    }
    seconds
}

extern "win64" fn _mktime64(t: *mut Tm) -> i64 {
    make(t, libc::mktime)
}

extern "win64" fn _mkgmtime64(t: *mut Tm) -> i64 {
    make(t, libc::timegm)
}

/// `strftime` on a narrow format, into `out` of `room` bytes: the bytes
/// written without the NUL, or 0 if they did not fit.
fn format(out: *mut u8, room: usize, format: &CString, t: *const Tm) -> usize {
    if t.is_null() || out.is_null() || room == 0 {
        set_errno(22);
        return 0;
    }
    // SAFETY: the caller's struct.
    let tm = to_libc(unsafe { &*t });
    // SAFETY: the caller's buffer of `room` bytes, our format and struct.
    unsafe { libc::strftime(out.cast(), room, format.as_ptr(), &tm) }
}

extern "win64" fn strftime(out: *mut u8, room: usize, fmt: *const c_char, t: *const Tm) -> usize {
    // SAFETY: the caller's format.
    let fmt = unsafe { std::ffi::CStr::from_ptr(fmt) }.to_owned();
    format(out, room, &fmt, t)
}

extern "win64" fn _Strftime(out: *mut u8, room: usize, fmt: *const c_char, t: *const Tm, _names: *const c_void) -> usize {
    strftime(out, room, fmt, t)
}

extern "win64" fn _Wcsftime(out: *mut u16, room: usize, fmt: *const u16, t: *const Tm, _names: *const c_void) -> usize {
    // SAFETY: the caller's format.
    let fmt = CString::new(unsafe { super::paths::wide(fmt) }).unwrap_or_default();
    let mut narrow = vec![0u8; room.saturating_mul(4).max(1)];
    let n = format(narrow.as_mut_ptr(), narrow.len(), &fmt, t);
    let wide: Vec<u16> = String::from_utf8_lossy(&narrow[..n]).encode_utf16().collect();
    if wide.len() >= room {
        return 0;
    }
    // SAFETY: the caller's buffer of `room` units.
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr(), out, wide.len());
        out.add(wide.len()).write(0);
    }
    wide.len()
}

const DAYS: [(&str, &str); 7] = [
    ("Sun", "Sunday"),
    ("Mon", "Monday"),
    ("Tue", "Tuesday"),
    ("Wed", "Wednesday"),
    ("Thu", "Thursday"),
    ("Fri", "Friday"),
    ("Sat", "Saturday"),
];
const MONTHS: [(&str, &str); 12] = [
    ("Jan", "January"),
    ("Feb", "February"),
    ("Mar", "March"),
    ("Apr", "April"),
    ("May", "May"),
    ("Jun", "June"),
    ("Jul", "July"),
    ("Aug", "August"),
    ("Sep", "September"),
    ("Oct", "October"),
    ("Nov", "November"),
    ("Dec", "December"),
];

/// `:Sun:Sunday:Mon:Monday...` in a block the caller frees.
fn names(list: &[(&str, &str)]) -> String {
    list.iter().map(|(short, long)| format!(":{short}:{long}")).collect()
}

fn malloc_narrow(text: &str) -> *mut c_char {
    // SAFETY: a block for the text and its NUL.
    unsafe {
        let p = libc::malloc(text.len() + 1).cast::<u8>();
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(text.as_ptr(), p, text.len());
            p.add(text.len()).write(0);
        }
        p.cast()
    }
}

fn malloc_wide(text: &str) -> *mut u16 {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    // SAFETY: a block for the units.
    unsafe {
        let p = libc::malloc(units.len() * 2).cast::<u16>();
        if !p.is_null() {
            std::ptr::copy_nonoverlapping(units.as_ptr(), p, units.len());
        }
        p
    }
}

extern "win64" fn _Getdays() -> *mut c_char {
    malloc_narrow(&names(&DAYS))
}

extern "win64" fn _Getmonths() -> *mut c_char {
    malloc_narrow(&names(&MONTHS))
}

extern "win64" fn _W_Getdays() -> *mut u16 {
    malloc_wide(&names(&DAYS))
}

extern "win64" fn _W_Getmonths() -> *mut u16 {
    malloc_wide(&names(&MONTHS))
}

/// `__lc_time_data` for the "C" locale, in a block the caller frees: the
/// C++ runtime hands it back to `_Strftime`, which does not read it.
/// ponytail: the names are not filled in; nothing reads them but the
/// Windows `_Strftime`.
extern "win64" fn _Gettnames() -> *mut c_void {
    // SAFETY: a zeroed block larger than the structure.
    unsafe { libc::calloc(1, 1024) }
}

extern "win64" fn _W_Gettnames() -> *mut c_void {
    _Gettnames()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_formats() {
        let epoch = 86_400 * 365;
        let mut tm = Tm::default();
        assert_eq!(_gmtime64_s(&mut tm, &epoch), 0);
        assert_eq!(tm.0[5], 71, "1971");
        assert_eq!(_mkgmtime64(&mut tm), epoch);
        let mut out = [0u8; 32];
        let n = strftime(out.as_mut_ptr(), 32, c"%Y-%m-%d".as_ptr(), &tm);
        assert_eq!(&out[..n], b"1971-01-01");
        let mut wide = [0u16; 32];
        let fmt: Vec<u16> = "%H:%M".encode_utf16().chain([0]).collect();
        let n = _Wcsftime(wide.as_mut_ptr(), 32, fmt.as_ptr(), &tm, std::ptr::null());
        assert_eq!(String::from_utf16_lossy(&wide[..n]), "00:00");
        assert!(names(&DAYS).starts_with(":Sun:Sunday:Mon"));
    }
}
