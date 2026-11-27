//! Strings and character classes (`api-ms-win-crt-string`). Wide strings
//! are 16-bit units here; macOS's `wchar_t` is 32 bits, so none of them go
//! to libc.
#![allow(non_snake_case)]

use std::ffi::c_char;

use super::locale::{class, classes};
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(strlen),
        hle_export!(strcmp),
        hle_export!(strncmp),
        hle_export!(strcpy),
        hle_export!(strncpy),
        hle_export!(strcat),
        hle_export!(strncat),
        hle_export!(strcspn),
        hle_export!(strpbrk),
        hle_export!(strtok),
        hle_export!(strtok_s),
        hle_export!(strcpy_s),
        hle_export!(strcat_s),
        hle_export!(strncpy_s),
        hle_export!(__strncnt),
        hle_export!(_stricmp),
        hle_export!(_strnicmp),
        hle_export!(wcslen),
        hle_export!(wcsnlen),
        hle_export!(wcscmp),
        hle_export!(wcscoll),
        hle_export!(_wcsicoll),
        hle_export!(_wcsnicmp),
        hle_export!(wcsncpy),
        hle_export!(wcscpy_s),
        hle_export!(wcscat_s),
        hle_export!(wcstok_s),
        hle_export!(_wcsdup),
        hle_export!(isalnum),
        hle_export!(isalpha),
        hle_export!(isdigit),
        hle_export!(islower),
        hle_export!(isupper),
        hle_export!(isspace),
        hle_export!(isxdigit),
        hle_export!(tolower),
        hle_export!(toupper),
        hle_export!(iswalnum),
        hle_export!(iswdigit),
        hle_export!(iswspace),
        hle_export!(iswxdigit),
    ]
}

const EINVAL: i32 = 22;
const ERANGE: i32 = 34;
const STRUNCATE: i32 = 80;
const TRUNCATE: usize = usize::MAX;

macro_rules! libc_same {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {$(
        extern "win64" fn $name($($arg: $ty),*) -> $ret {
            // SAFETY: the title's strings, as the C function takes them.
            unsafe { libc::$name($($arg),*) }
        }
    )*};
}

libc_same! {
    strlen(s: *const c_char) -> usize;
    strcmp(a: *const c_char, b: *const c_char) -> i32;
    strncmp(a: *const c_char, b: *const c_char, n: usize) -> i32;
    strcpy(to: *mut c_char, from: *const c_char) -> *mut c_char;
    strncpy(to: *mut c_char, from: *const c_char, n: usize) -> *mut c_char;
    strcat(to: *mut c_char, from: *const c_char) -> *mut c_char;
    strncat(to: *mut c_char, from: *const c_char, n: usize) -> *mut c_char;
    strcspn(s: *const c_char, reject: *const c_char) -> usize;
    strpbrk(s: *const c_char, accept: *const c_char) -> *mut c_char;
    strtok(s: *mut c_char, delim: *const c_char) -> *mut c_char;
    strtok_r(s: *mut c_char, delim: *const c_char, context: *mut *mut c_char) -> *mut c_char;
}

extern "win64" fn strtok_s(s: *mut c_char, delim: *const c_char, context: *mut *mut c_char) -> *mut c_char {
    strtok_r(s, delim, context)
}

/// Units of `s` before its NUL, at most `max`.
fn length<C: Copy + Default + PartialEq>(s: *const C, max: usize) -> usize {
    // SAFETY: the title's NUL-terminated string (or `max` readable units).
    (0..max).take_while(|&i| unsafe { *s.add(i) } != C::default()).count()
}

/// `strcpy_s` and its kin: `src[..len]` and a NUL into `to` of `size`
/// units, by the `_s` rules. `count` bounds what is copied (`TRUNCATE`:
/// cut to fit).
fn copy_s<C: Copy + Default + PartialEq>(to: *mut C, size: usize, at: usize, from: *const C, count: usize) -> i32 {
    if to.is_null() || size == 0 {
        return EINVAL;
    }
    if from.is_null() {
        // SAFETY: `to` holds `size` > 0 units.
        unsafe { to.write(C::default()) };
        return EINVAL;
    }
    let len = length(from, if count == TRUNCATE { usize::MAX } else { count });
    // SAFETY: inside `to`'s `size` units, as checked.
    unsafe {
        if at + len < size {
            std::ptr::copy_nonoverlapping(from, to.add(at), len);
            to.add(at + len).write(C::default());
            0
        } else if count == TRUNCATE && at < size {
            std::ptr::copy_nonoverlapping(from, to.add(at), size - 1 - at);
            to.add(size - 1).write(C::default());
            STRUNCATE
        } else {
            to.write(C::default());
            ERANGE
        }
    }
}

extern "win64" fn strcpy_s(to: *mut c_char, size: usize, from: *const c_char) -> i32 {
    copy_s(to, size, 0, from, TRUNCATE - 1)
}

extern "win64" fn strncpy_s(to: *mut c_char, size: usize, from: *const c_char, count: usize) -> i32 {
    copy_s(to, size, 0, from, count)
}

extern "win64" fn strcat_s(to: *mut c_char, size: usize, from: *const c_char) -> i32 {
    if to.is_null() {
        return EINVAL;
    }
    let at = length(to, size);
    if at == size {
        return EINVAL;
    }
    copy_s(to, size, at, from, TRUNCATE - 1)
}

extern "win64" fn __strncnt(s: *const c_char, count: usize) -> usize {
    length(s, count)
}

fn compare_folded(a: impl Iterator<Item = u32>, b: impl Iterator<Item = u32>) -> i32 {
    let fold = |c: u32| if (u32::from(b'A')..=u32::from(b'Z')).contains(&c) { c + 32 } else { c };
    let mut b = b;
    for x in a {
        let y = b.next().unwrap_or(0);
        let (x, y) = (fold(x), fold(y));
        if x != y || x == 0 {
            return x as i32 - y as i32;
        }
    }
    0
}

/// The units of a NUL-terminated string and its NUL, at most `max` units.
fn with_nul<C: Copy + Default + PartialEq + Into<u32>>(s: *const C, max: usize) -> impl Iterator<Item = u32> {
    let len = length(s, max);
    // SAFETY: `len` units before a NUL (or `max`).
    (0..len).map(move |i| unsafe { *s.add(i) }.into()).chain((len < max).then_some(0))
}

extern "win64" fn _stricmp(a: *const c_char, b: *const c_char) -> i32 {
    compare_folded(with_nul(a.cast::<u8>(), usize::MAX), with_nul(b.cast::<u8>(), usize::MAX))
}

extern "win64" fn _strnicmp(a: *const c_char, b: *const c_char, n: usize) -> i32 {
    compare_folded(with_nul(a.cast::<u8>(), n), with_nul(b.cast::<u8>(), n))
}

extern "win64" fn wcslen(s: *const u16) -> usize {
    length(s, usize::MAX)
}

extern "win64" fn wcsnlen(s: *const u16, max: usize) -> usize {
    length(s, max)
}

extern "win64" fn wcscmp(a: *const u16, b: *const u16) -> i32 {
    let mut b = with_nul(b, usize::MAX);
    for x in with_nul(a, usize::MAX) {
        let y = b.next().unwrap_or(0);
        if x != y || x == 0 {
            return (x > y) as i32 - (x < y) as i32;
        }
    }
    0
}

extern "win64" fn wcscoll(a: *const u16, b: *const u16) -> i32 {
    wcscmp(a, b)
}

extern "win64" fn _wcsicoll(a: *const u16, b: *const u16) -> i32 {
    compare_folded(with_nul(a, usize::MAX), with_nul(b, usize::MAX))
}

extern "win64" fn _wcsnicmp(a: *const u16, b: *const u16, n: usize) -> i32 {
    compare_folded(with_nul(a, n), with_nul(b, n))
}

extern "win64" fn wcsncpy(to: *mut u16, from: *const u16, n: usize) -> *mut u16 {
    let len = length(from, n);
    // SAFETY: the title's buffers, `n` units of `to`.
    unsafe {
        std::ptr::copy_nonoverlapping(from, to, len);
        std::ptr::write_bytes(to.add(len), 0, n - len);
    }
    to
}

extern "win64" fn wcscpy_s(to: *mut u16, size: usize, from: *const u16) -> i32 {
    copy_s(to, size, 0, from, TRUNCATE - 1)
}

extern "win64" fn wcscat_s(to: *mut u16, size: usize, from: *const u16) -> i32 {
    if to.is_null() {
        return EINVAL;
    }
    let at = length(to, size);
    if at == size {
        return EINVAL;
    }
    copy_s(to, size, at, from, TRUNCATE - 1)
}

extern "win64" fn wcstok_s(s: *mut u16, delim: *const u16, context: *mut *mut u16) -> *mut u16 {
    let delims: Vec<u16> = with_nul(delim, usize::MAX).filter(|&c| c != 0).map(|c| c as u16).collect();
    // SAFETY: the title's string, continued through `context`.
    unsafe {
        let mut p = if s.is_null() { context.read() } else { s };
        if p.is_null() {
            return std::ptr::null_mut();
        }
        while *p != 0 && delims.contains(&*p) {
            p = p.add(1);
        }
        if *p == 0 {
            context.write(p);
            return std::ptr::null_mut();
        }
        let token = p;
        while *p != 0 && !delims.contains(&*p) {
            p = p.add(1);
        }
        if *p != 0 {
            *p = 0;
            p = p.add(1);
        }
        context.write(p);
        token
    }
}

extern "win64" fn _wcsdup(s: *const u16) -> *mut u16 {
    let units = length(s, usize::MAX) + 1;
    // SAFETY: a new block for the string and its NUL.
    unsafe {
        let copy = libc::malloc(units * 2).cast::<u16>();
        if !copy.is_null() {
            std::ptr::copy_nonoverlapping(s, copy, units);
        }
        copy
    }
}

macro_rules! is {
    ($($name:ident = $bits:expr;)*) => {$(
        extern "win64" fn $name(c: i32) -> i32 {
            i32::from(classes(c) & ($bits))
        }
    )*};
}

is! {
    isalnum = class::ALPHA | class::DIGIT;
    isalpha = class::ALPHA;
    isdigit = class::DIGIT;
    islower = class::LOWER;
    isupper = class::UPPER;
    isspace = class::SPACE;
    isxdigit = class::HEX;
}

extern "win64" fn tolower(c: i32) -> i32 {
    if (i32::from(b'A')..=i32::from(b'Z')).contains(&c) { c + 32 } else { c }
}

extern "win64" fn toupper(c: i32) -> i32 {
    if (i32::from(b'a')..=i32::from(b'z')).contains(&c) { c - 32 } else { c }
}

fn wide_char(c: u16) -> Option<char> {
    char::from_u32(u32::from(c))
}

extern "win64" fn iswalnum(c: u16) -> i32 {
    i32::from(wide_char(c).is_some_and(char::is_alphanumeric))
}

extern "win64" fn iswdigit(c: u16) -> i32 {
    i32::from(wide_char(c).is_some_and(|c| c.is_ascii_digit()))
}

extern "win64" fn iswspace(c: u16) -> i32 {
    i32::from(wide_char(c).is_some_and(char::is_whitespace))
}

extern "win64" fn iswxdigit(c: u16) -> i32 {
    i32::from(wide_char(c).is_some_and(|c| c.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_copies_and_case_folding() {
        let mut buf = [0x55 as c_char; 4];
        assert_eq!(strcpy_s(buf.as_mut_ptr(), 4, c"abc".as_ptr()), 0);
        assert_eq!(strcpy_s(buf.as_mut_ptr(), 4, c"abcd".as_ptr()), ERANGE);
        assert_eq!(buf[0], 0, "emptied on overflow");
        assert_eq!(strncpy_s(buf.as_mut_ptr(), 4, c"abcdef".as_ptr(), TRUNCATE), STRUNCATE);
        assert_eq!(&buf, &[b'a' as c_char, b'b' as c_char, b'c' as c_char, 0]);
        assert_eq!(strcat_s(buf.as_mut_ptr(), 4, c"d".as_ptr()), ERANGE);
        assert_eq!(_stricmp(c"Hello".as_ptr(), c"hELLO".as_ptr()), 0);
        assert!(_stricmp(c"a".as_ptr(), c"B".as_ptr()) < 0);
        assert_eq!(_strnicmp(c"abcX".as_ptr(), c"ABCy".as_ptr(), 3), 0);
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        assert_eq!(_wcsnicmp(w("Data").as_ptr(), w("dATA\\x").as_ptr(), 4), 0);
        assert!(wcscmp(w("a").as_ptr(), w("ab").as_ptr()) < 0);
        let mut text = w("a,b,,c");
        let mut ctx = std::ptr::null_mut();
        let comma = w(",");
        let mut tokens = Vec::new();
        let mut p = wcstok_s(text.as_mut_ptr(), comma.as_ptr(), &mut ctx);
        while !p.is_null() {
            tokens.push(wcslen(p));
            p = wcstok_s(std::ptr::null_mut(), comma.as_ptr(), &mut ctx);
        }
        assert_eq!(tokens, [1, 1, 1]);
        assert_eq!(isalpha(i32::from(b'q')), i32::from(class::ALPHA));
        assert_eq!(isdigit(-1), 0, "EOF has no class");
        assert_eq!(toupper(i32::from(b'x')), i32::from(b'X'));
    }
}
