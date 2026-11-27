//! Number conversions (`api-ms-win-crt-convert`), sorting and random
//! numbers (`-utility`), and the C runtime's threads. Windows `long` is 32
//! bits: the `l` functions clamp to it, as Microsoft's do.
#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};

use super::crt::set_errno;
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(atoi),
        hle_export!(atol),
        hle_export!(atoll),
        hle_export!(atof),
        hle_export!(strtol),
        hle_export!(strtoul),
        hle_export!(strtoll),
        hle_export!(strtoull),
        hle_export!(_strtoi64),
        hle_export!(_strtoui64),
        hle_export!(strtod),
        hle_export!(strtof),
        hle_export!(_wtoi),
        hle_export!(wcstoull),
        hle_export!(btowc),
        hle_export!(wcstombs),
        hle_export!(_itoa),
        hle_export!(strerror),
        hle_export!(qsort),
        hle_export!(bsearch),
        hle_export!(rand),
        hle_export!(srand),
        hle_export!(rand_s),
        hle_export!(_beginthreadex),
        hle_export!(_endthreadex),
    ]
}

extern "win64" fn atoi(s: *const c_char) -> i32 {
    strtol(s, std::ptr::null_mut(), 10)
}

extern "win64" fn atol(s: *const c_char) -> i32 {
    strtol(s, std::ptr::null_mut(), 10)
}

extern "win64" fn atoll(s: *const c_char) -> i64 {
    strtoll(s, std::ptr::null_mut(), 10)
}

extern "win64" fn atof(s: *const c_char) -> f64 {
    strtod(s, std::ptr::null_mut())
}

/// The C library's own conversions, with errno copied over.
fn with_errno<T>(f: impl FnOnce() -> T) -> T {
    // SAFETY: the thread's errno.
    unsafe { *libc::__error() = 0 };
    let value = f();
    // SAFETY: as above.
    let e = unsafe { *libc::__error() };
    if e != 0 {
        set_errno(e);
    }
    value
}

extern "win64" fn strtoll(s: *const c_char, end: *mut *mut c_char, base: i32) -> i64 {
    // SAFETY: the caller's string and out pointer.
    with_errno(|| unsafe { libc::strtoll(s, end, base) })
}

extern "win64" fn strtoull(s: *const c_char, end: *mut *mut c_char, base: i32) -> u64 {
    // SAFETY: as above.
    with_errno(|| unsafe { libc::strtoull(s, end, base) })
}

extern "win64" fn _strtoi64(s: *const c_char, end: *mut *mut c_char, base: i32) -> i64 {
    strtoll(s, end, base)
}

extern "win64" fn _strtoui64(s: *const c_char, end: *mut *mut c_char, base: i32) -> u64 {
    strtoull(s, end, base)
}

extern "win64" fn strtol(s: *const c_char, end: *mut *mut c_char, base: i32) -> i32 {
    let v = strtoll(s, end, base);
    i32::try_from(v).unwrap_or_else(|_| {
        set_errno(34);
        if v < 0 { i32::MIN } else { i32::MAX }
    })
}

/// A negative number wraps, as C says, within 32 bits.
extern "win64" fn strtoul(s: *const c_char, end: *mut *mut c_char, base: i32) -> u32 {
    let v = strtoll(s, end, base);
    if (-(u32::MAX as i64)..=u32::MAX as i64).contains(&v) {
        v as u32
    } else {
        set_errno(34);
        u32::MAX
    }
}

extern "win64" fn strtod(s: *const c_char, end: *mut *mut c_char) -> f64 {
    // SAFETY: as above.
    with_errno(|| unsafe { libc::strtod(s, end) })
}

extern "win64" fn strtof(s: *const c_char, end: *mut *mut c_char) -> f32 {
    // SAFETY: as above.
    with_errno(|| unsafe { libc::strtof(s, end) })
}

/// An ASCII copy of a wide string (others end it), for libc.
fn ascii(s: *const u16) -> std::ffi::CString {
    let mut bytes = Vec::new();
    let mut i = 0;
    // SAFETY: the caller's NUL-terminated string.
    while let unit @ 1..=0x7f = unsafe { *s.add(i) } {
        bytes.push(unit as u8);
        i += 1;
    }
    std::ffi::CString::new(bytes).unwrap_or_default()
}

extern "win64" fn _wtoi(s: *const u16) -> i32 {
    atoi(ascii(s).as_ptr())
}

extern "win64" fn wcstoull(s: *const u16, end: *mut *mut u16, base: i32) -> u64 {
    let copy = ascii(s);
    let mut stop: *mut c_char = std::ptr::null_mut();
    let v = strtoull(copy.as_ptr(), &mut stop, base);
    if !end.is_null() {
        // SAFETY: `stop` points into `copy`; the same count of units in `s`.
        unsafe { end.write(s.add(stop.offset_from(copy.as_ptr()) as usize).cast_mut()) };
    }
    v
}

extern "win64" fn btowc(c: i32) -> u32 {
    if (0..0x80).contains(&c) { c as u32 } else { 0xffff }
}

extern "win64" fn wcstombs(to: *mut u8, from: *const u16, room: usize) -> usize {
    // SAFETY: the caller's string.
    let text = unsafe { super::paths::wide(from) };
    if to.is_null() {
        return text.len();
    }
    let n = text.len().min(room);
    // SAFETY: the caller's buffer of `room` bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(text.as_ptr(), to, n);
        if n < room {
            to.add(n).write(0);
        }
    }
    n
}

extern "win64" fn _itoa(value: i32, out: *mut u8, radix: i32) -> *mut u8 {
    let radix = radix.clamp(2, 36) as u32;
    // Base 10 is signed; others show the 32-bit pattern.
    let (negative, mut v) = if radix == 10 && value < 0 { (true, value.unsigned_abs()) } else { (false, value as u32) };
    let mut digits = Vec::new();
    loop {
        digits.push(char::from_digit(v % radix, radix).unwrap_or('0') as u8);
        v /= radix;
        if v == 0 {
            break;
        }
    }
    if negative {
        digits.push(b'-');
    }
    digits.reverse();
    digits.push(0);
    // SAFETY: the caller's buffer (33 bytes suffice for any 32-bit value).
    unsafe { std::ptr::copy_nonoverlapping(digits.as_ptr(), out, digits.len()) };
    out
}

/// The small error numbers are the same on both.
extern "win64" fn strerror(number: i32) -> *const c_char {
    // SAFETY: returns a static string.
    unsafe { libc::strerror(number) }
}

type Compare = unsafe extern "win64" fn(*const c_void, *const c_void) -> i32;

extern "C" fn compare_thunk(compare: *mut c_void, a: *const c_void, b: *const c_void) -> i32 {
    // SAFETY: the title's comparison function, passed through `qsort_r`.
    unsafe { std::mem::transmute::<*mut c_void, Compare>(compare)(a, b) }
}

unsafe extern "C" {
    fn qsort_r(base: *mut c_void, count: usize, size: usize, thunk: *mut c_void, compare: extern "C" fn(*mut c_void, *const c_void, *const c_void) -> i32);
}

extern "win64" fn qsort(base: *mut c_void, count: usize, size: usize, compare: Compare) {
    // SAFETY: the title's array of `count` elements of `size` bytes.
    unsafe { qsort_r(base, count, size, compare as *mut c_void, compare_thunk) }
}

extern "win64" fn bsearch(key: *const c_void, base: *const c_void, count: usize, size: usize, compare: Compare) -> *const c_void {
    let (mut low, mut high) = (0, count);
    while low < high {
        let mid = low + (high - low) / 2;
        // SAFETY: an element of the title's sorted array.
        let at = unsafe { base.cast::<u8>().add(mid * size) }.cast::<c_void>();
        match unsafe { compare(key, at) } {
            0 => return at,
            c if c < 0 => high = mid,
            _ => low = mid + 1,
        }
    }
    std::ptr::null()
}

thread_local! {
    /// Microsoft's `rand` state, per thread, seeded 1.
    static SEED: std::cell::Cell<u32> = const { std::cell::Cell::new(1) };
}

extern "win64" fn srand(seed: u32) {
    SEED.set(seed);
}

/// Microsoft's generator: the same numbers as on the console.
extern "win64" fn rand() -> i32 {
    let next = SEED.get().wrapping_mul(214_013).wrapping_add(2_531_011);
    SEED.set(next);
    (next >> 16 & 0x7fff) as i32
}

extern "win64" fn rand_s(out: *mut u32) -> i32 {
    if out.is_null() {
        return 22;
    }
    unsafe extern "C" {
        fn arc4random() -> u32;
    }
    // SAFETY: a plain call and the caller's out pointer.
    unsafe { out.write(arc4random()) };
    0
}

use super::objects::ThreadStart;

/// `_beginthreadex`: `CreateThread` with the C runtime's arguments.
extern "win64" fn _beginthreadex(security: *const c_void, stack: u32, start: ThreadStart, arg: *mut c_void, flags: u32, id: *mut u32) -> usize {
    super::objects::create_thread(security, stack as usize, start, arg, flags, id)
}

extern "win64" fn _endthreadex(code: u32) -> ! {
    super::objects::exit_thread(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "win64" fn by_value(a: *const c_void, b: *const c_void) -> i32 {
        // SAFETY: two i32 elements.
        unsafe { (*a.cast::<i32>()).cmp(&*b.cast::<i32>()) as i32 }
    }

    #[test]
    fn conversions_sorting_and_rand() {
        assert_eq!(strtol(c"99999999999".as_ptr(), std::ptr::null_mut(), 10), i32::MAX, "clamped to 32 bits");
        assert_eq!(strtoul(c"-1".as_ptr(), std::ptr::null_mut(), 10), u32::MAX, "wraps in 32 bits");
        assert_eq!(atoi(c" 42x".as_ptr()), 42);
        let mut buf = [0u8; 34];
        _itoa(-255, buf.as_mut_ptr(), 10);
        assert_eq!(&buf[..5], b"-255\0");
        _itoa(-1, buf.as_mut_ptr(), 16);
        assert_eq!(&buf[..9], b"ffffffff\0");
        let mut v = [5i32, 1, 4, 2, 3];
        qsort(v.as_mut_ptr().cast(), 5, 4, by_value);
        assert_eq!(v, [1, 2, 3, 4, 5]);
        let key = 4i32;
        let found = bsearch((&raw const key).cast(), v.as_ptr().cast(), 5, 4, by_value);
        assert_eq!(found, (&raw const v[3]).cast());
        srand(1);
        assert_eq!([rand(), rand(), rand()], [41, 18467, 6334], "Microsoft's first numbers");
    }
}
