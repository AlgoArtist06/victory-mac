//! `printf` with the Universal C runtime's rules, for narrow (`u8`) and wide
//! (`u16`) strings. Arguments come from a Windows `va_list`: a pointer to
//! 8-byte slots, floating point values as doubles.
//!
//! Text is ours (strings, characters, pointers); numbers are formatted one
//! conversion at a time by libc's `snprintf`, which has C's rounding and
//! exponent rules.

use std::ffi::c_char;

/// `_CRT_INTERNAL_PRINTF_LEGACY_WIDE_SPECIFIERS`: in wide functions `%s` is
/// a wide string (the MSVC default) rather than a narrow one (ISO).
pub(crate) const LEGACY_WIDE_SPECIFIERS: u64 = 1 << 2;

/// A Windows `va_list`.
pub(crate) struct Args(pub(crate) *const u64);

impl Args {
    pub(crate) fn next(&mut self) -> u64 {
        // SAFETY: the caller's argument list holds as many slots as its
        // format asks for.
        let value = unsafe { self.0.read_unaligned() };
        // SAFETY: as above.
        self.0 = unsafe { self.0.add(1) };
        value
    }

    pub(crate) fn next_f64(&mut self) -> f64 {
        f64::from_bits(self.next())
    }
}

/// A code unit of a format string and its output.
pub(crate) trait Unit: Copy + Eq + Default + 'static {
    fn code(self) -> u32;
    fn from_ascii(b: u8) -> Self;
    /// Appends `text`, converted to this width.
    fn push_str(out: &mut Vec<Self>, text: &str);
    const WIDE: bool;
}

impl Unit for u8 {
    fn code(self) -> u32 {
        u32::from(self)
    }
    fn from_ascii(b: u8) -> Self {
        b
    }
    fn push_str(out: &mut Vec<Self>, text: &str) {
        out.extend_from_slice(text.as_bytes());
    }
    const WIDE: bool = false;
}

impl Unit for u16 {
    fn code(self) -> u32 {
        u32::from(self)
    }
    fn from_ascii(b: u8) -> Self {
        u16::from(b)
    }
    fn push_str(out: &mut Vec<Self>, text: &str) {
        out.extend(text.encode_utf16());
    }
    const WIDE: bool = true;
}

/// The units of a NUL-terminated string, at most `max`.
///
/// # Safety
/// `p` points at a NUL-terminated string (or `max` readable units).
pub(crate) unsafe fn units<'a, C: Unit>(p: *const C, max: usize) -> &'a [C] {
    // SAFETY: guaranteed by the caller.
    let len = (0..max).take_while(|&i| unsafe { *p.add(i) } != C::default()).count();
    // SAFETY: as above.
    unsafe { std::slice::from_raw_parts(p, len) }
}

#[derive(Clone, Copy, PartialEq)]
enum Size {
    Default,
    Char,  // hh
    Short, // h
    Long,  // l (32 bits on Windows)
    Wide,  // ll, I64, I, z, t, j
}

/// Formats `format` with `args`. `legacy_wide`: see [`LEGACY_WIDE_SPECIFIERS`].
///
/// # Safety
/// `format` is NUL-terminated and `args` holds what it asks for.
pub(crate) unsafe fn format<C: Unit>(format: *const C, args: &mut Args, legacy_wide: bool) -> Vec<C> {
    // SAFETY: guaranteed by the caller.
    let f = unsafe { units(format, usize::MAX) };
    let mut out: Vec<C> = Vec::with_capacity(f.len() + 16);
    let mut i = 0;
    let at = |i: usize| f.get(i).map_or(0, |c| c.code());
    while i < f.len() {
        if at(i) != u32::from(b'%') {
            out.push(f[i]);
            i += 1;
            continue;
        }
        i += 1;
        if at(i) == u32::from(b'%') {
            out.push(C::from_ascii(b'%'));
            i += 1;
            continue;
        }
        let mut flags = String::new();
        while let Some(c) = char::from_u32(at(i)).filter(|c| "-+ #0".contains(*c)) {
            flags.push(c);
            i += 1;
        }
        let mut number = |i: &mut usize, args: &mut Args| -> Option<i64> {
            if at(*i) == u32::from(b'*') {
                *i += 1;
                return Some(i64::from(args.next() as i32));
            }
            let start = *i;
            let mut n = 0i64;
            while (u32::from(b'0')..=u32::from(b'9')).contains(&at(*i)) {
                n = n * 10 + i64::from(at(*i) - u32::from(b'0'));
                *i += 1;
            }
            (*i > start).then_some(n)
        };
        let mut width = number(&mut i, args);
        if let Some(w) = width.filter(|w| *w < 0) {
            flags.push('-');
            width = Some(-w);
        }
        let precision = if at(i) == u32::from(b'.') {
            i += 1;
            Some(number(&mut i, args).unwrap_or(0)).filter(|p| *p >= 0)
        } else {
            None
        };
        let mut size = Size::Default;
        let mut wide_arg: Option<bool> = None;
        loop {
            let rest: String = (i..i + 3).filter_map(|k| char::from_u32(at(k))).collect();
            let (step, s) = if rest.starts_with("I64") {
                (3, Size::Wide)
            } else if rest.starts_with("I32") {
                (3, Size::Long)
            } else if rest.starts_with("hh") {
                (2, Size::Char)
            } else if rest.starts_with("ll") {
                (2, Size::Wide)
            } else {
                match rest.chars().next() {
                    Some('h') => {
                        wide_arg = Some(false);
                        (1, Size::Short)
                    }
                    Some('l') => {
                        wide_arg = Some(true);
                        (1, Size::Long)
                    }
                    Some('w') => {
                        wide_arg = Some(true);
                        (1, Size::Default)
                    }
                    Some('I' | 'z' | 't' | 'j') => (1, Size::Wide),
                    Some('L') => (1, Size::Default),
                    _ => break,
                }
            };
            size = if s == Size::Default { size } else { s };
            i += step;
        }
        let Some(conversion) = char::from_u32(at(i)) else { break };
        i += 1;
        let mut text: Vec<C> = Vec::new();
        let mut pad_zero_ok = true;
        match conversion {
            'd' | 'i' | 'u' | 'o' | 'x' | 'X' => {
                let raw = args.next();
                let signed = matches!(conversion, 'd' | 'i');
                let value: i64 = match (size, signed) {
                    (Size::Char, true) => i64::from(raw as i8),
                    (Size::Short, true) => i64::from(raw as i16),
                    (Size::Default | Size::Long, true) => i64::from(raw as i32),
                    (Size::Char, false) => i64::from(raw as u8),
                    (Size::Short, false) => i64::from(raw as u16),
                    (Size::Default | Size::Long, false) => i64::from(raw as u32),
                    (Size::Wide, _) => raw as i64,
                };
                let spec = c_spec(&flags, width, precision, "ll", conversion);
                C::push_str(&mut text, &c_format_int(&spec, value));
                width = None;
            }
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'a' | 'A' => {
                let value = args.next_f64();
                let spec = c_spec(&flags, width, precision, "", conversion);
                C::push_str(&mut text, &c_format_float(&spec, value));
                width = None;
            }
            'p' => C::push_str(&mut text, &format!("{:016X}", args.next())),
            'c' | 'C' => {
                // In narrow functions `%c` is narrow and `%C` wide; in wide
                // ones the other way round. `h`/`l` choose outright.
                let wide = wide_arg.unwrap_or((conversion == 'C') != C::WIDE);
                let code = args.next() as u32;
                let ch = if wide {
                    char::decode_utf16([code as u16]).next().and_then(Result::ok)
                } else {
                    char::from_u32(code & 0xff)
                };
                C::push_str(&mut text, &ch.unwrap_or('\u{fffd}').to_string());
                pad_zero_ok = false;
            }
            's' | 'S' | 'Z' => {
                let wide = wide_arg.unwrap_or(if C::WIDE {
                    (conversion == 's') == legacy_wide
                } else {
                    conversion == 'S'
                });
                let p = args.next() as usize;
                let max = precision.map_or(usize::MAX, |p| p as usize);
                let s = if p == 0 {
                    "(null)".chars().take(max).collect()
                } else if wide {
                    // SAFETY: the title's wide string.
                    String::from_utf16_lossy(unsafe { units(p as *const u16, max) })
                } else {
                    // SAFETY: the title's narrow string.
                    String::from_utf8_lossy(unsafe { units(p as *const u8, max) }).into_owned()
                };
                C::push_str(&mut text, &s);
                pad_zero_ok = false;
            }
            'n' => {
                // Disabled by default in the Universal C runtime.
                tracing::warn!("printf %n ignored");
                args.next();
            }
            other => {
                tracing::warn!(conversion = %other, "printf conversion not understood");
                C::push_str(&mut text, &format!("%{other}"));
            }
        }
        if let Some(w) = width.map(|w| w as usize).filter(|w| *w > text.len()) {
            let pad = w - text.len();
            let fill = C::from_ascii(if flags.contains('0') && pad_zero_ok { b'0' } else { b' ' });
            if flags.contains('-') {
                text.extend(std::iter::repeat_n(C::from_ascii(b' '), pad));
            } else {
                text.splice(0..0, std::iter::repeat_n(fill, pad));
            }
        }
        out.extend(text);
    }
    out
}

/// A C conversion specification, e.g. `%-08.3llx`.
fn c_spec(flags: &str, width: Option<i64>, precision: Option<i64>, length: &str, conversion: char) -> String {
    let mut spec = format!("%{flags}");
    if let Some(w) = width {
        spec += &w.to_string();
    }
    if let Some(p) = precision {
        spec += &format!(".{p}");
    }
    spec += length;
    spec.push(conversion);
    spec
}

fn c_format_int(spec: &str, value: i64) -> String {
    let spec = std::ffi::CString::new(spec).unwrap_or_default();
    let mut buf = vec![0u8; 128];
    // SAFETY: our buffer and a spec with one `ll` integer conversion.
    let n = unsafe { libc::snprintf(buf.as_mut_ptr().cast::<c_char>(), buf.len(), spec.as_ptr(), value) };
    buf.truncate(n.clamp(0, 127) as usize);
    String::from_utf8_lossy(&buf).into_owned()
}

fn c_format_float(spec: &str, value: f64) -> String {
    let spec = std::ffi::CString::new(spec).unwrap_or_default();
    // SAFETY: a spec with one double conversion; first asked for its length.
    unsafe {
        let n = libc::snprintf(std::ptr::null_mut(), 0, spec.as_ptr(), value).max(0) as usize;
        let mut buf = vec![0u8; n + 1];
        libc::snprintf(buf.as_mut_ptr().cast::<c_char>(), buf.len(), spec.as_ptr(), value);
        buf.truncate(n);
        String::from_utf8_lossy(&buf).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn narrow(f: &str, slots: &[u64]) -> String {
        let f = std::ffi::CString::new(f).unwrap();
        // SAFETY: a NUL-terminated format and enough slots.
        let out = unsafe { format(f.as_ptr().cast::<u8>(), &mut Args(slots.as_ptr()), true) };
        String::from_utf8(out).unwrap()
    }

    fn wide(f: &str, slots: &[u64]) -> String {
        let f: Vec<u16> = f.encode_utf16().chain([0]).collect();
        // SAFETY: as above.
        let out = unsafe { format(f.as_ptr(), &mut Args(slots.as_ptr()), true) };
        String::from_utf16(&out).unwrap()
    }

    #[test]
    fn formats_as_the_universal_c_runtime_does() {
        let s = c"abc";
        let w: Vec<u16> = "wide".encode_utf16().chain([0]).collect();
        assert_eq!(narrow("%d|%5d|%-5d|%05d", &[-3i64 as u64, 42, 7, 9]), "-3|   42|7    |00009");
        assert_eq!(narrow("%x %X %#x %o", &[255, 255, 255, 8]), "ff FF 0xff 10");
        assert_eq!(narrow("%u", &[u64::MAX]), "4294967295", "plain %u is 32 bits");
        assert_eq!(narrow("%lu %ld", &[u64::MAX, u64::MAX]), "4294967295 -1", "long is 32 bits");
        assert_eq!(narrow("%llu %I64d %zu", &[u64::MAX, u64::MAX, 5]), "18446744073709551615 -1 5");
        assert_eq!(narrow("%.3f %e %g", &[1.5f64.to_bits(), 100000.0f64.to_bits(), 0.0001f64.to_bits()]), "1.500 1.000000e+05 0.0001");
        assert_eq!(narrow("%*.*f", &[8, 2, 3.14159f64.to_bits()]), "    3.14");
        assert_eq!(narrow("[%s] [%.2s] [%6s] [%-6s]", &[s.as_ptr() as u64, s.as_ptr() as u64, s.as_ptr() as u64, s.as_ptr() as u64]), "[abc] [ab] [   abc] [abc   ]");
        assert_eq!(narrow("%S %ls %c%%", &[w.as_ptr() as u64, w.as_ptr() as u64, u64::from(b'z')]), "wide wide z%");
        assert_eq!(narrow("%p", &[0x1_4000_a000]), "000000014000A000");
        assert_eq!(narrow("%s", &[0]), "(null)");
        assert_eq!(wide("%s|%S|%hs|%c", &[w.as_ptr() as u64, s.as_ptr() as u64, s.as_ptr() as u64, u64::from(b'q')]), "wide|abc|abc|q");
        assert_eq!(wide("%d%%", &[50]), "50%");
    }
}
