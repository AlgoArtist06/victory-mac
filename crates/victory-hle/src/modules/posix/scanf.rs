//! `sscanf` with the Universal C runtime's rules, for narrow and wide input.
//! Numbers are read by libc's `strtoll`, `strtoull` and `strtod` on a copy
//! of the input, bounded by the field width.

use std::ffi::{CString, c_char};

use super::printf::{Args, Unit, units};

/// `_CRT_INTERNAL_SCANF_SECURECRT`: `%s`, `%c` and `%[` are followed by
/// their buffer's size (`sscanf_s`).
pub(crate) const SECURE: u64 = 1 << 0;
/// `_CRT_INTERNAL_SCANF_LEGACY_WIDE_SPECIFIERS`.
pub(crate) const LEGACY_WIDE_SPECIFIERS: u64 = 1 << 1;

fn is_space(c: u32) -> bool {
    matches!(c, 0x20 | 0x09..=0x0d)
}

/// Scans `input` by `format`, storing through the pointers in `args`.
/// The number of fields stored, or -1 if the input ended first.
///
/// # Safety
/// Both strings are NUL-terminated (the input at most `len` units), and
/// `args` holds the pointers the format asks for.
pub(crate) unsafe fn scan<C: Unit>(input: *const C, len: usize, format: *const C, args: &mut Args, options: u64) -> i32 {
    // SAFETY: guaranteed by the caller.
    let (s, f) = unsafe { (units(input, len), units(format, usize::MAX)) };
    let secure = options & SECURE != 0;
    let legacy_wide = options & LEGACY_WIDE_SPECIFIERS != 0;
    let (mut i, mut k, mut stored) = (0usize, 0usize, 0i32);
    let get = |x: &[C], at: usize| x.get(at).map_or(0, |c| c.code());
    let ended = |stored: i32| if stored == 0 { -1 } else { stored };
    while k < f.len() {
        let fc = get(f, k);
        if is_space(fc) {
            while is_space(get(s, i)) {
                i += 1;
            }
            k += 1;
            continue;
        }
        if fc != u32::from(b'%') || get(f, k + 1) == u32::from(b'%') {
            if fc == u32::from(b'%') {
                k += 1;
                while is_space(get(s, i)) {
                    i += 1;
                }
            }
            if i >= s.len() {
                return ended(stored);
            }
            if get(s, i) != get(f, k) {
                return stored;
            }
            i += 1;
            k += 1;
            continue;
        }
        k += 1;
        let skip = get(f, k) == u32::from(b'*');
        if skip {
            k += 1;
        }
        let mut width = 0usize;
        while (u32::from(b'0')..=u32::from(b'9')).contains(&get(f, k)) {
            width = width * 10 + (get(f, k) - u32::from(b'0')) as usize;
            k += 1;
        }
        let mut bytes = 4usize;
        let mut wide_arg: Option<bool> = None;
        loop {
            match char::from_u32(get(f, k)) {
                Some('h') if get(f, k + 1) == u32::from(b'h') => {
                    bytes = 1;
                    k += 2;
                }
                Some('h') => {
                    bytes = 2;
                    wide_arg = Some(false);
                    k += 1;
                }
                Some('l') if get(f, k + 1) == u32::from(b'l') => {
                    bytes = 8;
                    k += 2;
                }
                Some('l') => {
                    // `long` stays 4 bytes; `%lf` is a double, `%ls` wide.
                    bytes = 4;
                    wide_arg = Some(true);
                    k += 1;
                }
                Some('I') if get(f, k + 1) == u32::from(b'6') && get(f, k + 2) == u32::from(b'4') => {
                    bytes = 8;
                    k += 3;
                }
                Some('I' | 'z' | 't' | 'j') => {
                    bytes = 8;
                    k += 1;
                }
                Some('L') => {
                    bytes = 8;
                    k += 1;
                }
                Some('w') => {
                    wide_arg = Some(true);
                    k += 1;
                }
                _ => break,
            }
        }
        let Some(conversion) = char::from_u32(get(f, k)) else { return stored };
        k += 1;
        let limit = if width == 0 { usize::MAX } else { width };
        if !matches!(conversion, 'c' | '[' | 'n') {
            while is_space(get(s, i)) {
                i += 1;
            }
        }
        if conversion == 'n' {
            if !skip {
                // SAFETY: the caller's int pointer.
                unsafe { (args.next() as *mut i32).write(i as i32) };
            }
            continue;
        }
        if i >= s.len() {
            return ended(stored);
        }
        match conversion {
            'd' | 'i' | 'u' | 'o' | 'x' | 'X' | 'p' => {
                let base = match conversion {
                    'd' | 'u' => 10,
                    'i' => 0,
                    'o' => 8,
                    _ => 16,
                };
                let text = ascii(&s[i..], limit);
                let Some((value, used)) = strto(&text, |p, end| {
                    // SAFETY: a NUL-terminated copy and an out pointer.
                    unsafe {
                        if conversion == 'd' || conversion == 'i' {
                            libc::strtoll(p, end, base) as u64
                        } else {
                            libc::strtoull(p, end, base)
                        }
                    }
                }) else {
                    return stored;
                };
                i += used;
                if !skip {
                    let bytes = if conversion == 'p' { 8 } else { bytes };
                    let to = args.next() as *mut u8;
                    // SAFETY: the caller's pointer to an integer of `bytes`.
                    unsafe { std::ptr::copy_nonoverlapping(value.to_le_bytes().as_ptr(), to, bytes) };
                    stored += 1;
                }
            }
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => {
                let text = ascii(&s[i..], limit);
                // SAFETY: as above.
                let Some((bits, used)) = strto(&text, |p, end| unsafe { libc::strtod(p, end).to_bits() }) else {
                    return stored;
                };
                i += used;
                if !skip {
                    let value = f64::from_bits(bits);
                    let to = args.next();
                    // SAFETY: the caller's pointer to a float or double.
                    unsafe {
                        if wide_arg == Some(true) || bytes == 8 {
                            (to as *mut f64).write_unaligned(value);
                        } else {
                            (to as *mut f32).write_unaligned(value as f32);
                        }
                    }
                    stored += 1;
                }
            }
            's' | 'S' | 'c' | 'C' | '[' => {
                let set = if conversion == '[' {
                    let negate = get(f, k) == u32::from(b'^');
                    if negate {
                        k += 1;
                    }
                    let start = k;
                    if get(f, k) == u32::from(b']') {
                        k += 1;
                    }
                    while k < f.len() && get(f, k) != u32::from(b']') {
                        k += 1;
                    }
                    let members: Vec<u32> = f[start..k].iter().map(|c| c.code()).collect();
                    k += 1;
                    Some((negate, members))
                } else {
                    None
                };
                let accepts = |c: u32| match &set {
                    Some((negate, members)) => in_set(members, c) != *negate,
                    None if conversion == 'c' || conversion == 'C' => true,
                    None => !is_space(c),
                };
                let limit = if width == 0 && matches!(conversion, 'c' | 'C') { 1 } else { limit };
                let start = i;
                while i < s.len() && i - start < limit && accepts(get(s, i)) {
                    i += 1;
                }
                if i == start {
                    return stored;
                }
                if !skip {
                    let wide_out = wide_arg.unwrap_or(match conversion {
                        'S' | 'C' => !C::WIDE,
                        _ if C::WIDE => legacy_wide,
                        _ => false,
                    });
                    let to = args.next();
                    let room = if secure { args.next() as u32 as usize } else { usize::MAX };
                    let terminate = !matches!(conversion, 'c' | 'C');
                    let codes: Vec<u32> = s[start..i].iter().map(|c| c.code()).collect();
                    if codes.len() + usize::from(terminate) > room {
                        return stored;
                    }
                    // SAFETY: the caller's buffer, `room` units if secure.
                    unsafe {
                        if wide_out {
                            let text: String = codes.iter().filter_map(|&c| char::from_u32(c)).collect();
                            let mut out: Vec<u16> = if C::WIDE { codes.iter().map(|&c| c as u16).collect() } else { text.encode_utf16().collect() };
                            if terminate {
                                out.push(0);
                            }
                            std::ptr::copy_nonoverlapping(out.as_ptr(), to as *mut u16, out.len());
                        } else {
                            let mut out: Vec<u8> = codes.iter().map(|&c| c as u8).collect();
                            if terminate {
                                out.push(0);
                            }
                            std::ptr::copy_nonoverlapping(out.as_ptr(), to as *mut u8, out.len());
                        }
                    }
                    stored += 1;
                }
            }
            other => {
                tracing::warn!(conversion = %other, "scanf conversion not understood");
                return stored;
            }
        }
    }
    stored
}

/// A `[...]` set's members, with `a-z` ranges.
fn in_set(members: &[u32], c: u32) -> bool {
    let mut j = 0;
    while j < members.len() {
        if j + 2 < members.len() && members[j + 1] == u32::from(b'-') {
            if (members[j]..=members[j + 2]).contains(&c) {
                return true;
            }
            j += 3;
        } else {
            if members[j] == c {
                return true;
            }
            j += 1;
        }
    }
    false
}

/// At most `limit` units of `s` as ASCII for libc (others end it).
fn ascii<C: Unit>(s: &[C], limit: usize) -> CString {
    let bytes: Vec<u8> = s
        .iter()
        .take(limit)
        .map(|c| c.code())
        .take_while(|&c| c != 0 && c < 0x80)
        .map(|c| c as u8)
        .collect();
    CString::new(bytes).unwrap_or_default()
}

/// Runs a libc `strto*` on `text`: its value and the units it used, or
/// `None` if it used none.
fn strto(text: &CString, f: impl FnOnce(*const c_char, *mut *mut c_char) -> u64) -> Option<(u64, usize)> {
    let mut end: *mut c_char = std::ptr::null_mut();
    let value = f(text.as_ptr(), &mut end);
    // SAFETY: `end` points into `text`.
    let used = unsafe { end.offset_from(text.as_ptr()) } as usize;
    (used > 0).then_some((value, used))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scans_as_the_universal_c_runtime_does() {
        let (mut a, mut b, mut x) = (0i32, 0i64, 0u32);
        let mut f = 0f32;
        let mut d = 0f64;
        let mut word = [0u8; 8];
        let mut rest = [0u8; 16];
        let input = c"  -12 345678901234 ff 2.5 0.125 hello [abc]";
        let format = c"%d %lld %x %f %lf %7s [%[a-z]]";
        let slots = [
            &mut a as *mut i32 as u64,
            &mut b as *mut i64 as u64,
            &mut x as *mut u32 as u64,
            &mut f as *mut f32 as u64,
            &mut d as *mut f64 as u64,
            word.as_mut_ptr() as u64,
            rest.as_mut_ptr() as u64,
        ];
        // SAFETY: NUL-terminated strings and pointers to our locals.
        let n = unsafe {
            scan(input.as_ptr().cast::<u8>(), usize::MAX, format.as_ptr().cast::<u8>(), &mut Args(slots.as_ptr()), 0)
        };
        assert_eq!(n, 7);
        assert_eq!((a, b, x, f, d), (-12, 345678901234, 0xff, 2.5, 0.125));
        assert_eq!(&word[..6], b"hello\0");
        assert_eq!(&rest[..4], b"abc\0");
        let mut small = [0u8; 4];
        let secure = [small.as_mut_ptr() as u64, 4];
        // SAFETY: as above; the size follows the buffer.
        let n = unsafe { scan(c"toolong".as_ptr().cast::<u8>(), usize::MAX, c"%s".as_ptr().cast::<u8>(), &mut Args(secure.as_ptr()), SECURE) };
        assert_eq!(n, 0, "a string longer than its buffer is not stored");
        // SAFETY: as above.
        let n = unsafe { scan(c"".as_ptr().cast::<u8>(), usize::MAX, c"%d".as_ptr().cast::<u8>(), &mut Args([0u64].as_ptr()), 0) };
        assert_eq!(n, -1, "input ended before the first field");
    }
}
