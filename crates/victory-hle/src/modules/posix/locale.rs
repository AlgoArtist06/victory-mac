//! The C runtime's locale (`api-ms-win-crt-locale`) and character classes:
//! the "C" locale only, which a Windows program starts in.
#![allow(non_snake_case)]

use std::ffi::c_char;

use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(_lock_locales),
        hle_export!(_unlock_locales),
        hle_export!(___lc_codepage_func),
        hle_export!(___lc_collate_cp_func),
        hle_export!(___lc_locale_name_func),
        hle_export!(___mb_cur_max_func),
        hle_export!(__pctype_func),
        hle_export!(_configthreadlocale),
        hle_export!(localeconv),
        hle_export!(setlocale),
    ]
}

/// The C runtime's character class bits.
pub(crate) mod class {
    pub const UPPER: u16 = 0x1;
    pub const LOWER: u16 = 0x2;
    pub const DIGIT: u16 = 0x4;
    pub const SPACE: u16 = 0x8;
    pub const PUNCT: u16 = 0x10;
    pub const CONTROL: u16 = 0x20;
    pub const BLANK: u16 = 0x40;
    pub const HEX: u16 = 0x80;
    pub const ALPHA: u16 = 0x100;
}

/// The "C" locale's class of each byte, after one entry for EOF (-1).
pub(crate) static CLASSES: [u16; 257] = {
    use class::*;
    let mut table = [0u16; 257];
    let mut c = 0;
    while c < 128 {
        let b = c as u8;
        let mut bits = 0;
        if b.is_ascii_uppercase() {
            bits |= UPPER | ALPHA;
        }
        if b.is_ascii_lowercase() {
            bits |= LOWER | ALPHA;
        }
        if b.is_ascii_digit() {
            bits |= DIGIT;
        }
        if b.is_ascii_hexdigit() {
            bits |= HEX;
        }
        if matches!(b, b' ' | 0x09..=0x0d) {
            bits |= SPACE;
        }
        if b.is_ascii_punctuation() {
            bits |= PUNCT;
        }
        if b.is_ascii_control() {
            bits |= CONTROL;
        }
        if b == b' ' || b == b'\t' {
            bits |= BLANK;
        }
        table[c + 1] = bits;
        c += 1;
    }
    table
};

/// The class bits of `c` (an `unsigned char` value or EOF) in the "C" locale.
pub(crate) fn classes(c: i32) -> u16 {
    CLASSES.get((c + 1) as usize).copied().unwrap_or(0)
}

/// ponytail: the locale never changes, so there is nothing to guard.
extern "win64" fn _lock_locales() {}

extern "win64" fn _unlock_locales() {}

extern "win64" fn ___lc_codepage_func() -> u32 {
    0
}

extern "win64" fn ___lc_collate_cp_func() -> u32 {
    0
}

/// The locale names per category: none in the "C" locale.
extern "win64" fn ___lc_locale_name_func() -> *const *const u16 {
    static NAMES: [usize; 6] = [0; 6];
    NAMES.as_ptr().cast()
}

extern "win64" fn ___mb_cur_max_func() -> i32 {
    1
}

extern "win64" fn __pctype_func() -> *const u16 {
    CLASSES[1..].as_ptr()
}

/// Per-thread locales are off (`_DISABLE_PER_THREAD_LOCALE`).
extern "win64" fn _configthreadlocale(_: i32) -> i32 {
    2
}

/// The Universal C runtime's `struct lconv`.
#[repr(C)]
struct Lconv {
    narrow: [*const c_char; 10],
    numbers: [c_char; 8],
    wide: [*const u16; 8],
}
// SAFETY: pointers to static strings only.
unsafe impl Sync for Lconv {}

static POINT: [u16; 2] = [b'.' as u16, 0];
static EMPTY: [u16; 1] = [0];

extern "win64" fn localeconv() -> *const Lconv {
    static C: Lconv = Lconv {
        narrow: [
            c".".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
            c"".as_ptr(),
        ],
        numbers: [127; 8],
        wide: [
            POINT.as_ptr(),
            EMPTY.as_ptr(),
            EMPTY.as_ptr(),
            EMPTY.as_ptr(),
            EMPTY.as_ptr(),
            EMPTY.as_ptr(),
            EMPTY.as_ptr(),
            EMPTY.as_ptr(),
        ],
    };
    &C
}

/// Every locale asked for is the "C" one.
extern "win64" fn setlocale(_category: i32, locale: *const c_char) -> *const c_char {
    if !locale.is_null() {
        // SAFETY: the title's string.
        let asked = unsafe { std::ffi::CStr::from_ptr(locale) };
        if !matches!(asked.to_bytes(), b"" | b"C") {
            tracing::debug!(locale = %asked.to_string_lossy(), "setlocale: staying with the C locale");
        }
    }
    c"C".as_ptr()
}
