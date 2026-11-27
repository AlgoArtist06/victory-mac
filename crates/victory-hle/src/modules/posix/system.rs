//! System facts and small services: the registry (empty), event tracing
//! (off), system information, time conversions, text conversion and
//! locale, COM's basics, random numbers, and stand-ins for window and
//! process calls a console title makes.
#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};

use super::misc::filetime_now;
use super::{Bool, FALSE, TRUE, error, set_last_error};
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(EventRegister),
        hle_export!(EventUnregister),
        hle_export!(EventSetInformation),
        hle_export!(EventWriteTransfer),
        hle_export!(WerRegisterFile),
        hle_export!(GetNativeSystemInfo),
        hle_export!(GlobalMemoryStatusEx),
        hle_export!(GetLogicalProcessorInformation),
        hle_export!(GetSystemTime),
        hle_export!(GetLocalTime),
        hle_export!(FileTimeToSystemTime),
        hle_export!(SystemTimeToFileTime),
        hle_export!(FileTimeToLocalFileTime),
        hle_export!(LocalFileTimeToFileTime),
        hle_export!(GetDateFormatEx),
        hle_export!(GetTimeFormatEx),
        hle_export!(MultiByteToWideChar),
        hle_export!(WideCharToMultiByte),
        hle_export!(GetCPInfo),
        hle_export!(GetLocaleInfoEx),
        hle_export!(GetUserDefaultLocaleName),
        hle_export!(GetUserDefaultGeoName),
        hle_export!(LocaleNameToLCID),
        hle_export!(LCMapStringEx),
        hle_export!(CompareStringEx),
        hle_export!(GetStringTypeW),
        hle_export!(lstrcmpW),
        hle_export!(FormatMessageA),
        hle_export!(CoInitializeEx),
        hle_export!(CoUninitialize),
        hle_export!(CoCreateGuid),
        hle_export!(StringFromGUID2),
        hle_export!(CoTaskMemFree),
        hle_export!(CoCreateInstance),
        hle_export!(PropVariantClear),
        hle_export!(BCryptOpenAlgorithmProvider),
        hle_export!(BCryptCloseAlgorithmProvider),
        hle_export!(BCryptGenRandom),
        hle_export!(GetStartupInfoW),
        hle_export!(GetCommandLineA),
        hle_export!(GetProcessId),
        hle_export!(GetThreadPriority),
        hle_export!(OpenThread),
        hle_export!(SuspendThread),
        hle_export!(SetThreadAffinityMask),
        hle_export!(GetThreadGroupAffinity),
        hle_export!(SetThreadGroupAffinity),
        hle_export!(SetThreadIdealProcessorEx),
        hle_export!(SetProcessPriorityBoost),
        hle_export!(RegisterAppStateChangeNotification),
        hle_export!(UnregisterAppStateChangeNotification),
        hle_export!(RegisterAppConstrainedChangeNotification),
        hle_export!(UnregisterAppConstrainedChangeNotification),
        hle_export!(UnhandledExceptionFilter),
        hle_export!(GlobalFree),
        hle_export!(LocalFree),
        hle_export!(GetModuleHandleExW),
        hle_export!(CreateWindowExA),
        hle_export!(RegisterClassExA),
        hle_export!(DefWindowProcA),
        hle_export!(ShowWindow),
        hle_export!(PeekMessageA),
        hle_export!(DispatchMessageA),
        hle_export!(TranslateMessage),
        hle_export!(SetNamedPipeHandleState),
        hle_export!(WaitNamedPipeW),
    ]
}

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;

/// Event tracing: registered, and every event dropped.
extern "win64" fn EventRegister(_: *const c_void, _: usize, _: *mut c_void, handle: *mut u64) -> u32 {
    // SAFETY: the caller's out pointer.
    unsafe { handle.write(1) };
    0
}

extern "win64" fn EventUnregister(_: u64) -> u32 {
    0
}

extern "win64" fn EventSetInformation(_: u64, _: u32, _: *const c_void, _: u32) -> u32 {
    0
}

extern "win64" fn EventWriteTransfer(_: u64, _: *const c_void, _: *const c_void, _: *const c_void, _: u32, _: *const c_void) -> u32 {
    0
}

extern "win64" fn WerRegisterFile(_: *const u16, _: u32, _: u32) -> i32 {
    0
}

/// Logical processors of this Mac.
fn processors() -> u32 {
    std::thread::available_parallelism().map_or(8, |n| n.get() as u32)
}

#[repr(C)]
struct SystemInfo {
    architecture: u16,
    reserved: u16,
    page_size: u32,
    lowest: usize,
    highest: usize,
    active_mask: usize,
    processors: u32,
    processor_type: u32,
    granularity: u32,
    level: u16,
    revision: u16,
}

extern "win64" fn GetNativeSystemInfo(out: *mut SystemInfo) {
    let n = processors();
    // SAFETY: the caller's struct.
    unsafe {
        out.write(SystemInfo {
            architecture: 9, // PROCESSOR_ARCHITECTURE_AMD64
            reserved: 0,
            page_size: 4096,
            lowest: 0x10000,
            highest: 0x7fff_fffe_ffff,
            active_mask: (1usize << n) - 1,
            processors: n,
            processor_type: 8664,
            granularity: 65536,
            level: 23, // Zen 2
            revision: 0x7100,
        })
    };
}

fn physical_memory() -> u64 {
    let mut bytes = 0u64;
    let mut len = size_of::<u64>();
    // SAFETY: a plain query into our variable.
    unsafe { libc::sysctlbyname(c"hw.memsize".as_ptr(), (&raw mut bytes).cast(), &mut len, std::ptr::null_mut(), 0) };
    bytes
}

#[repr(C)]
struct MemoryStatus {
    length: u32,
    load: u32,
    total_physical: u64,
    available_physical: u64,
    total_page_file: u64,
    available_page_file: u64,
    total_virtual: u64,
    available_virtual: u64,
    available_extended: u64,
}

extern "win64" fn GlobalMemoryStatusEx(out: *mut MemoryStatus) -> Bool {
    let total = physical_memory();
    let mut level = 50u32;
    let mut len = size_of::<u32>();
    // SAFETY: a plain query (percent of memory available).
    unsafe { libc::sysctlbyname(c"kern.memorystatus_level".as_ptr(), (&raw mut level).cast(), &mut len, std::ptr::null_mut(), 0) };
    let available = total / 100 * u64::from(level.min(100));
    // SAFETY: the caller's struct.
    unsafe {
        out.write(MemoryStatus {
            length: size_of::<MemoryStatus>() as u32,
            load: 100 - level.min(100),
            total_physical: total,
            available_physical: available,
            total_page_file: total * 2,
            available_page_file: available + total,
            total_virtual: 1 << 47,
            available_virtual: 1 << 46,
            available_extended: 0,
        })
    };
    TRUE
}

/// One entry per core (`RelationProcessorCore`), each one logical processor.
extern "win64" fn GetLogicalProcessorInformation(buffer: *mut [u64; 4], length: *mut u32) -> Bool {
    let n = processors() as usize;
    let need = (n * 32) as u32;
    // SAFETY: the caller's length and buffer of that many bytes.
    unsafe {
        if buffer.is_null() || length.read() < need {
            length.write(need);
            set_last_error(ERROR_INSUFFICIENT_BUFFER);
            return FALSE;
        }
        for i in 0..n {
            buffer.add(i).write([1 << i, 0, 0, 0]);
        }
        length.write(need);
    }
    TRUE
}

/// `SYSTEMTIME`.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct SystemTime {
    year: u16,
    month: u16,
    weekday: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    milliseconds: u16,
}

/// Days since 1970-01-01 of a civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

const TICKS_PER_DAY: i64 = 864_000_000_000;
const DAYS_1601_TO_1970: i64 = 134_774;

fn to_system_time(filetime: u64) -> SystemTime {
    let ticks = filetime as i64;
    let days = ticks.div_euclid(TICKS_PER_DAY) - DAYS_1601_TO_1970;
    let rest = ticks.rem_euclid(TICKS_PER_DAY);
    let (y, m, d) = civil_from_days(days);
    SystemTime {
        year: y as u16,
        month: m as u16,
        weekday: (days + 4).rem_euclid(7) as u16,
        day: d as u16,
        hour: (rest / 36_000_000_000) as u16,
        minute: (rest / 600_000_000 % 60) as u16,
        second: (rest / 10_000_000 % 60) as u16,
        milliseconds: (rest / 10_000 % 1000) as u16,
    }
}

fn to_filetime(t: &SystemTime) -> u64 {
    let days = days_from_civil(i64::from(t.year), i64::from(t.month), i64::from(t.day)) + DAYS_1601_TO_1970;
    let ms = ((i64::from(t.hour) * 60 + i64::from(t.minute)) * 60 + i64::from(t.second)) * 1000 + i64::from(t.milliseconds);
    (days * TICKS_PER_DAY + ms * 10_000) as u64
}

/// This Mac's offset from UTC now, in FILETIME ticks.
fn local_offset() -> i64 {
    let now = (filetime_now() / 10_000_000) as i64 - 11_644_473_600;
    // SAFETY: plain conversion into our struct.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&now, &mut tm) };
    tm.tm_gmtoff * 10_000_000
}

extern "win64" fn GetSystemTime(out: *mut SystemTime) {
    // SAFETY: the caller's struct.
    unsafe { out.write(to_system_time(filetime_now())) };
}

extern "win64" fn GetLocalTime(out: *mut SystemTime) {
    // SAFETY: as above.
    unsafe { out.write(to_system_time(filetime_now().wrapping_add_signed(local_offset()))) };
}

extern "win64" fn FileTimeToSystemTime(filetime: *const u64, out: *mut SystemTime) -> Bool {
    // SAFETY: the caller's FILETIME (4-byte aligned) and struct.
    unsafe { out.write(to_system_time(filetime.read_unaligned())) };
    TRUE
}

extern "win64" fn SystemTimeToFileTime(time: *const SystemTime, out: *mut u64) -> Bool {
    // SAFETY: as above.
    unsafe { out.write_unaligned(to_filetime(&*time)) };
    TRUE
}

extern "win64" fn FileTimeToLocalFileTime(utc: *const u64, out: *mut u64) -> Bool {
    // SAFETY: as above.
    unsafe { out.write_unaligned(utc.read_unaligned().wrapping_add_signed(local_offset())) };
    TRUE
}

extern "win64" fn LocalFileTimeToFileTime(local: *const u64, out: *mut u64) -> Bool {
    // SAFETY: as above.
    unsafe { out.write_unaligned(local.read_unaligned().wrapping_add_signed(-local_offset())) };
    TRUE
}

/// Writes `text` and a NUL to a wide buffer of `len` units: the units
/// written (with the NUL), the units needed if `len` is 0, or 0 with
/// `ERROR_INSUFFICIENT_BUFFER`.
fn put_wide(text: &str, out: *mut u16, len: i32) -> i32 {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    if len == 0 {
        return units.len() as i32;
    }
    if (len as usize) < units.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    // SAFETY: the caller's buffer of `len` units.
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), out, units.len()) };
    units.len() as i32
}

/// A date or time in a Windows picture (`yyyy`, `MM`, `dd`, `HH`, `hh`,
/// `mm`, `ss`, `tt`); other letters are kept.
fn picture(format: &str, t: &SystemTime) -> String {
    let mut out = String::new();
    let chars: Vec<char> = format.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let run = chars[i..].iter().take_while(|&&x| x == c).count();
        let text = match (c, run) {
            ('y', 4..) => format!("{:04}", t.year),
            ('y', _) => format!("{:02}", t.year % 100),
            ('M', 2..) => format!("{:02}", t.month),
            ('M', _) => t.month.to_string(),
            ('d', 2) => format!("{:02}", t.day),
            ('d', 1) => t.day.to_string(),
            ('H', 2..) => format!("{:02}", t.hour),
            ('H', _) => t.hour.to_string(),
            ('h', 2..) => format!("{:02}", (t.hour + 11) % 12 + 1),
            ('h', _) => ((t.hour + 11) % 12 + 1).to_string(),
            ('m', 2..) => format!("{:02}", t.minute),
            ('m', _) => t.minute.to_string(),
            ('s', 2..) => format!("{:02}", t.second),
            ('s', _) => t.second.to_string(),
            ('t', _) => (if t.hour < 12 { "AM" } else { "PM" })[..run.min(2)].to_string(),
            _ => chars[i..i + run].iter().collect(),
        };
        out += &text;
        i += run;
    }
    out
}

fn now_or(time: *const SystemTime) -> SystemTime {
    if time.is_null() {
        to_system_time(filetime_now().wrapping_add_signed(local_offset()))
    } else {
        // SAFETY: the caller's struct.
        unsafe { time.read() }
    }
}

extern "win64" fn GetDateFormatEx(_: *const u16, _: u32, time: *const SystemTime, format: *const u16, out: *mut u16, len: i32, _: *const u16) -> i32 {
    // SAFETY: the caller's string.
    let format = if format.is_null() { "M/d/yyyy".to_string() } else { unsafe { super::paths::wide(format) } };
    put_wide(&picture(&format, &now_or(time)), out, len)
}

extern "win64" fn GetTimeFormatEx(_: *const u16, _: u32, time: *const SystemTime, format: *const u16, out: *mut u16, len: i32) -> i32 {
    // SAFETY: the caller's string.
    let format = if format.is_null() { "h:mm:ss tt".to_string() } else { unsafe { super::paths::wide(format) } };
    put_wide(&picture(&format, &now_or(time)), out, len)
}

/// Narrow text is taken as UTF-8 in every code page.
extern "win64" fn MultiByteToWideChar(_page: u32, _flags: u32, from: *const u8, len: i32, to: *mut u16, room: i32) -> i32 {
    // SAFETY: the caller's string: NUL-terminated if `len` is -1 (the NUL
    // is converted too), else `len` bytes.
    let bytes = unsafe {
        if len < 0 {
            std::slice::from_raw_parts(from, libc::strlen(from.cast()) + 1)
        } else {
            std::slice::from_raw_parts(from, len as usize)
        }
    };
    let units: Vec<u16> = String::from_utf8_lossy(bytes).encode_utf16().collect();
    if room == 0 {
        return units.len() as i32;
    }
    if (room as usize) < units.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    // SAFETY: the caller's buffer of `room` units.
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), to, units.len()) };
    units.len() as i32
}

extern "win64" fn WideCharToMultiByte(
    _page: u32,
    _flags: u32,
    from: *const u16,
    len: i32,
    to: *mut u8,
    room: i32,
    _default: *const c_char,
    used_default: *mut Bool,
) -> i32 {
    // SAFETY: as above, in units.
    let units = unsafe {
        if len < 0 {
            let n = (0..).take_while(|&i| *from.add(i) != 0).count();
            std::slice::from_raw_parts(from, n + 1)
        } else {
            std::slice::from_raw_parts(from, len as usize)
        }
    };
    let text = String::from_utf16_lossy(units);
    if !used_default.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { used_default.write(FALSE) };
    }
    if room == 0 {
        return text.len() as i32;
    }
    if (room as usize) < text.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    // SAFETY: the caller's buffer of `room` bytes.
    unsafe { std::ptr::copy_nonoverlapping(text.as_ptr(), to, text.len()) };
    text.len() as i32
}

#[repr(C)]
struct CpInfo {
    max_char_size: u32,
    default_char: [u8; 2],
    lead_byte: [u8; 12],
}

extern "win64" fn GetCPInfo(page: u32, out: *mut CpInfo) -> Bool {
    // SAFETY: the caller's struct.
    unsafe {
        out.write(CpInfo {
            max_char_size: if page == 65001 { 4 } else { 1 },
            default_char: [b'?', 0],
            lead_byte: [0; 12],
        })
    };
    TRUE
}

const LOCALE: &str = "en-US";

extern "win64" fn GetLocaleInfoEx(_name: *const u16, kind: u32, out: *mut u16, len: i32) -> i32 {
    const RETURN_NUMBER: u32 = 0x2000_0000;
    let (number, text) = match kind & !RETURN_NUMBER & !0x8000_0000 {
        0x5c => (0, LOCALE),        // LOCALE_SNAME
        0x59 => (0, "en"),          // LOCALE_SISO639LANGNAME
        0x5a => (0, "US"),          // LOCALE_SISO3166CTRYNAME
        0x1004 => (1252, "1252"),   // LOCALE_IDEFAULTANSICODEPAGE
        0xb => (437, "437"),        // LOCALE_IDEFAULTCODEPAGE
        0xe => (0, "."),            // LOCALE_SDECIMAL
        0xf => (0, ","),            // LOCALE_STHOUSAND
        0x1 => (0x409, "0409"),     // LOCALE_ILANGUAGE
        _ => (0, ""),
    };
    if kind & RETURN_NUMBER != 0 {
        if len < 2 {
            return 2;
        }
        // SAFETY: room for a u32 in the caller's buffer.
        unsafe { out.cast::<u32>().write_unaligned(number) };
        return 2;
    }
    put_wide(text, out, len)
}

extern "win64" fn GetUserDefaultLocaleName(out: *mut u16, len: i32) -> i32 {
    put_wide(LOCALE, out, len)
}

extern "win64" fn GetUserDefaultGeoName(out: *mut u16, len: i32) -> i32 {
    put_wide("US", out, len)
}

extern "win64" fn LocaleNameToLCID(_: *const u16, _: u32) -> u32 {
    0x409
}

/// The units of a wide string of `len` units (-1: NUL-terminated).
fn wide_units<'a>(p: *const u16, len: i32) -> &'a [u16] {
    // SAFETY: the caller's string.
    unsafe {
        let n = if len < 0 { (0..).take_while(|&i| *p.add(i) != 0).count() } else { len as usize };
        std::slice::from_raw_parts(p, n)
    }
}

extern "win64" fn LCMapStringEx(_: *const u16, flags: u32, from: *const u16, len: i32, to: *mut u16, room: i32, _: *const c_void, _: *const c_void, _: isize) -> i32 {
    let text = String::from_utf16_lossy(wide_units(from, len));
    let mapped = match flags & 0x300 {
        0x100 => text.to_lowercase(),
        0x200 => text.to_uppercase(),
        _ => text,
    };
    let units: Vec<u16> = mapped.encode_utf16().chain((len < 0).then_some(0)).collect();
    if room == 0 {
        return units.len() as i32;
    }
    if (room as usize) < units.len() {
        set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return 0;
    }
    // SAFETY: the caller's buffer of `room` units.
    unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), to, units.len()) };
    units.len() as i32
}

extern "win64" fn CompareStringEx(_: *const u16, flags: u32, a: *const u16, a_len: i32, b: *const u16, b_len: i32, _: *const c_void, _: *const c_void, _: isize) -> i32 {
    let fold = |p, n| {
        let s = String::from_utf16_lossy(wide_units(p, n));
        if flags & 0x11 != 0 { s.to_lowercase() } else { s }
    };
    match fold(a, a_len).cmp(&fold(b, b_len)) {
        std::cmp::Ordering::Less => 1,
        std::cmp::Ordering::Equal => 2,
        std::cmp::Ordering::Greater => 3,
    }
}

extern "win64" fn GetStringTypeW(kind: u32, from: *const u16, len: i32, out: *mut u16) -> Bool {
    if kind != 1 {
        set_last_error(error::INVALID_PARAMETER);
        return FALSE;
    }
    for (i, &unit) in wide_units(from, len).iter().enumerate() {
        let c = char::from_u32(u32::from(unit)).unwrap_or('\0');
        let mut bits = 0x200u16; // C1_DEFINED
        for (yes, bit) in [
            (c.is_uppercase(), 0x1),
            (c.is_lowercase(), 0x2),
            (c.is_ascii_digit(), 0x4),
            (c.is_whitespace(), 0x8),
            (c.is_ascii_punctuation(), 0x10),
            (c.is_control(), 0x20),
            (c == ' ' || c == '\t', 0x40),
            (c.is_ascii_hexdigit(), 0x80),
            (c.is_alphabetic(), 0x100),
        ] {
            if yes {
                bits |= bit;
            }
        }
        // SAFETY: the caller's array, one entry per unit.
        unsafe { out.add(i).write(bits) };
    }
    TRUE
}

extern "win64" fn lstrcmpW(a: *const u16, b: *const u16) -> i32 {
    match String::from_utf16_lossy(wide_units(a, -1)).cmp(&String::from_utf16_lossy(wide_units(b, -1))) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// No message tables: callers fall back to the number.
extern "win64" fn FormatMessageA(_: u32, _: *const c_void, _: u32, _: u32, _: *mut c_char, _: u32, _: *const c_void) -> u32 {
    set_last_error(317); // ERROR_MR_MID_NOT_FOUND
    0
}

extern "win64" fn CoInitializeEx(_: *mut c_void, _: u32) -> i32 {
    0
}

extern "win64" fn CoUninitialize() {}

fn random_bytes(to: *mut u8, len: usize) {
    unsafe extern "C" {
        fn arc4random_buf(buf: *mut c_void, len: usize);
    }
    // SAFETY: the caller's buffer of `len` bytes.
    unsafe { arc4random_buf(to.cast(), len) };
}

/// A random (version 4) GUID.
extern "win64" fn CoCreateGuid(guid: *mut [u8; 16]) -> i32 {
    random_bytes(guid.cast(), 16);
    // SAFETY: the caller's GUID.
    unsafe {
        (*guid)[7] = (*guid)[7] & 0x0f | 0x40;
        (*guid)[8] = (*guid)[8] & 0x3f | 0x80;
    }
    0
}

extern "win64" fn StringFromGUID2(guid: *const [u8; 16], out: *mut u16, len: i32) -> i32 {
    // SAFETY: the caller's GUID.
    let g = unsafe { *guid };
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    let text = format!(
        "{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    );
    if len < 39 {
        return 0;
    }
    put_wide(&text, out, len)
}

extern "win64" fn CoTaskMemFree(p: *mut c_void) {
    // SAFETY: a block from our heap (CoTaskMemAlloc is libc's malloc here).
    unsafe { libc::free(p) };
}

/// No COM classes are registered.
extern "win64" fn CoCreateInstance(class: *const [u8; 16], _: *mut c_void, _: u32, iid: *const [u8; 16], out: *mut *mut c_void) -> i32 {
    let text = |g: *const [u8; 16]| {
        let mut out = [0u16; 39];
        StringFromGUID2(g, out.as_mut_ptr(), 39);
        String::from_utf16_lossy(&out[..38])
    };
    // SAFETY: the caller's class id.
    if !class.is_null() && unsafe { *class } == super::wasapi::CLSID_MM_DEVICE_ENUMERATOR {
        return super::wasapi::create_enumerator(iid, out);
    }
    tracing::info!(class = text(class), iid = text(iid), "CoCreateInstance: no such class");
    if !out.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { out.write(std::ptr::null_mut()) };
    }
    0x8004_0154_u32 as i32 // REGDB_E_CLASSNOTREG
}

extern "win64" fn PropVariantClear(p: *mut [u64; 3]) -> i32 {
    // SAFETY: the caller's PROPVARIANT (24 bytes).
    unsafe { p.write([0; 3]) };
    0
}

extern "win64" fn BCryptOpenAlgorithmProvider(handle: *mut usize, _: *const u16, _: *const u16, _: u32) -> i32 {
    // SAFETY: the caller's out pointer.
    unsafe { handle.write(1) };
    0
}

extern "win64" fn BCryptCloseAlgorithmProvider(_: usize, _: u32) -> i32 {
    0
}

extern "win64" fn BCryptGenRandom(_: usize, to: *mut u8, len: u32, _: u32) -> i32 {
    random_bytes(to, len as usize);
    0
}

extern "win64" fn GetStartupInfoW(out: *mut [u8; 104]) {
    // SAFETY: the caller's STARTUPINFOW (104 bytes; `cb` first).
    unsafe {
        out.write([0; 104]);
        out.cast::<u32>().write(104);
    }
}

extern "win64" fn GetCommandLineA() -> *const c_char {
    c"game_scarlett_master.exe".as_ptr()
}

extern "win64" fn GetProcessId(_: usize) -> u32 {
    std::process::id()
}

extern "win64" fn GetThreadPriority(_: usize) -> i32 {
    0
}

/// Other threads are not opened by id.
extern "win64" fn OpenThread(_: u32, _: Bool, _: u32) -> usize {
    set_last_error(error::INVALID_PARAMETER);
    0
}

extern "win64" fn SuspendThread(_: usize) -> u32 {
    set_last_error(error::INVALID_HANDLE);
    u32::MAX
}

/// ponytail: affinity and ideal processors are hints macOS does not take.
extern "win64" fn SetThreadAffinityMask(_: usize, _: usize) -> usize {
    (1usize << processors()) - 1
}

extern "win64" fn GetThreadGroupAffinity(_: usize, out: *mut [usize; 2]) -> Bool {
    // SAFETY: the caller's GROUP_AFFINITY (mask, group, reserved).
    unsafe { out.write([(1usize << processors()) - 1, 0]) };
    TRUE
}

extern "win64" fn SetThreadGroupAffinity(_: usize, _: *const c_void, previous: *mut [usize; 2]) -> Bool {
    if !previous.is_null() {
        GetThreadGroupAffinity(0, previous);
    }
    TRUE
}

extern "win64" fn SetThreadIdealProcessorEx(_: usize, _: *const c_void, _: *mut c_void) -> Bool {
    TRUE
}

extern "win64" fn SetProcessPriorityBoost(_: usize, _: Bool) -> Bool {
    TRUE
}

/// The title is never suspended or constrained here, so its callbacks are
/// kept but never called.
extern "win64" fn RegisterAppStateChangeNotification(_: usize, _: *mut c_void, registration: *mut usize) -> u32 {
    // SAFETY: the caller's out pointer.
    unsafe { registration.write(1) };
    0
}

extern "win64" fn UnregisterAppStateChangeNotification(_: usize) {}

extern "win64" fn RegisterAppConstrainedChangeNotification(_: usize, _: *mut c_void, registration: *mut usize) -> u32 {
    // SAFETY: the caller's out pointer.
    unsafe { registration.write(1) };
    0
}

extern "win64" fn UnregisterAppConstrainedChangeNotification(_: usize) {}

/// `EXCEPTION_CONTINUE_SEARCH`: the caller ends the process itself.
extern "win64" fn UnhandledExceptionFilter(_: *mut c_void) -> i32 {
    0
}

extern "win64" fn GlobalFree(p: *mut c_void) -> *mut c_void {
    // SAFETY: a block from our heap, or null.
    unsafe { libc::free(p) };
    std::ptr::null_mut()
}

extern "win64" fn LocalFree(p: *mut c_void) -> *mut c_void {
    GlobalFree(p)
}

const FROM_ADDRESS: u32 = 4;

extern "win64" fn GetModuleHandleExW(flags: u32, name: *const u16, module: *mut usize) -> Bool {
    let found = if flags & FROM_ADDRESS != 0 {
        victory_loader::image_containing(name as usize).map_or(0, |(base, _)| base)
    } else {
        let by_name = crate::registry::lookup("kernel32.dll", "GetModuleHandleW").unwrap_or(0);
        if by_name == 0 {
            0
        } else {
            // SAFETY: our own GetModuleHandleW.
            let f: extern "win64" fn(*const u16) -> usize = unsafe { std::mem::transmute(by_name) };
            f(name)
        }
    };
    // SAFETY: the caller's out pointer.
    unsafe { module.write(found) };
    if found == 0 {
        set_last_error(126); // ERROR_MOD_NOT_FOUND
        FALSE
    } else {
        TRUE
    }
}

/// Window calls: the title's window is a stand-in; the screen is ours.
const WINDOW: usize = 0x1000;

extern "win64" fn CreateWindowExA(_: u32, _: *const c_char, _: *const c_char, _: u32, _: i32, _: i32, _: i32, _: i32, _: usize, _: usize, _: usize, _: *const c_void) -> usize {
    WINDOW
}

extern "win64" fn RegisterClassExA(_: *const c_void) -> u16 {
    1
}

extern "win64" fn DefWindowProcA(_: usize, _: u32, _: usize, _: isize) -> isize {
    0
}

extern "win64" fn ShowWindow(_: usize, _: i32) -> Bool {
    FALSE
}

extern "win64" fn PeekMessageA(_: *mut c_void, _: usize, _: u32, _: u32, _: u32) -> Bool {
    FALSE
}

extern "win64" fn DispatchMessageA(_: *const c_void) -> isize {
    0
}

extern "win64" fn TranslateMessage(_: *const c_void) -> Bool {
    FALSE
}

extern "win64" fn SetNamedPipeHandleState(_: usize, _: *const u32, _: *const u32, _: *const u32) -> Bool {
    set_last_error(error::INVALID_HANDLE);
    FALSE
}

extern "win64" fn WaitNamedPipeW(_: *const u16, _: u32) -> Bool {
    set_last_error(ERROR_FILE_NOT_FOUND);
    FALSE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_convert_both_ways() {
        // 2026-11-27 07:31:05.250 UTC, a Friday.
        let t = SystemTime { year: 2026, month: 11, weekday: 5, day: 27, hour: 7, minute: 31, second: 5, milliseconds: 250 };
        let ft = to_filetime(&t);
        assert_eq!(to_system_time(ft), t);
        assert_eq!(to_system_time(116_444_736_000_000_000).year, 1970, "the Unix epoch");
        assert_eq!(picture("yyyy-MM-dd HH:mm:ss", &t), "2026-11-27 07:31:05");
        assert_eq!(picture("h:mm tt", &t), "7:31 AM");
        let mut out = [0u16; 8];
        assert_eq!(MultiByteToWideChar(65001, 0, b"hi\0".as_ptr(), -1, out.as_mut_ptr(), 8), 3);
        assert_eq!(&out[..3], &[104, 105, 0]);
        let mut back = [0u8; 8];
        assert_eq!(WideCharToMultiByte(65001, 0, out.as_ptr(), -1, back.as_mut_ptr(), 8, std::ptr::null(), std::ptr::null_mut()), 3);
        assert_eq!(&back[..3], b"hi\0");
        let mut guid_text = [0u16; 39];
        let guid = [0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a, 0xf0, 0xde, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(StringFromGUID2(&guid, guid_text.as_mut_ptr(), 39), 39);
        assert_eq!(String::from_utf16_lossy(&guid_text[..38]), "{12345678-9ABC-DEF0-0102-030405060708}");
    }
}
