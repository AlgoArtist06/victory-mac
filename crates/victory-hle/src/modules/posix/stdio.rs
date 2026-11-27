//! The C runtime's streams and files (`api-ms-win-crt-stdio`,
//! `-filesystem`): a `FILE` of ours around libc's, low-level descriptors,
//! and the `printf`/`scanf` entry points.
//!
//! ponytail: streams are binary; text mode's `\r\n` translation is not done.
#![allow(non_snake_case)]

use std::ffi::{CString, c_char, c_void};
use std::sync::LazyLock;

use super::crt::set_errno;
use super::paths::{host_path, narrow, wide};
use super::printf::{self, Args, Unit};
use super::scanf;
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(__acrt_iob_func),
        hle_export!(__p__commode),
        hle_export!(_set_fmode),
        hle_export!(__stdio_common_vfprintf),
        hle_export!(__stdio_common_vfwprintf),
        hle_export!(__stdio_common_vsprintf),
        hle_export!(__stdio_common_vsprintf_s),
        hle_export!(__stdio_common_vsnprintf_s),
        hle_export!(__stdio_common_vswprintf),
        hle_export!(__stdio_common_vswprintf_s),
        hle_export!(__stdio_common_vsnwprintf_s),
        hle_export!(__stdio_common_vsscanf),
        hle_export!(__stdio_common_vswscanf),
        hle_export!(fopen),
        hle_export!(_fsopen),
        hle_export!(_wfopen),
        hle_export!(_wfsopen),
        hle_export!(fclose),
        hle_export!(fread),
        hle_export!(fwrite),
        hle_export!(fgetc),
        hle_export!(fputc),
        hle_export!(fputs),
        hle_export!(ungetc),
        hle_export!(fgetwc),
        hle_export!(fputwc),
        hle_export!(ungetwc),
        hle_export!(fflush),
        hle_export!(ferror),
        hle_export!(fseek),
        hle_export!(ftell),
        hle_export!(_fseeki64),
        hle_export!(_ftelli64),
        hle_export!(fgetpos),
        hle_export!(fsetpos),
        hle_export!(rewind),
        hle_export!(setvbuf),
        hle_export!(_get_stream_buffer_pointers),
        hle_export!(_lock_file),
        hle_export!(_unlock_file),
        hle_export!(_open),
        hle_export!(_wopen),
        hle_export!(_read),
        hle_export!(_write),
        hle_export!(_close),
        hle_export!(_lseeki64),
        hle_export!(_wstat64),
        hle_export!(_wremove),
        hle_export!(remove),
        hle_export!(_wrename),
        hle_export!(_wrmdir),
        hle_export!(_wchdir),
    ]
}

/// The title's `FILE`. The first three fields are what
/// `_get_stream_buffer_pointers` hands out (the C++ runtime's file streams
/// read and write through them): kept empty, so every access comes to us.
#[repr(C)]
pub(crate) struct File {
    ptr: *mut u8,
    base: *mut u8,
    count: i32,
    inner: *mut libc::FILE,
}

fn new_file(inner: *mut libc::FILE) -> *mut File {
    if inner.is_null() {
        set_errno(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
        return std::ptr::null_mut();
    }
    Box::into_raw(Box::new(File {
        ptr: std::ptr::null_mut(),
        base: std::ptr::null_mut(),
        count: 0,
        inner,
    }))
}

/// libc's stream behind the title's.
fn inner(file: *mut File) -> *mut libc::FILE {
    // SAFETY: a FILE we made.
    unsafe { (*file).inner }
}

static STANDARD: LazyLock<[usize; 3]> = LazyLock::new(|| {
    // SAFETY: the process's standard descriptors.
    unsafe {
        [
            new_file(libc::fdopen(0, c"r".as_ptr())) as usize,
            new_file(libc::fdopen(1, c"w".as_ptr())) as usize,
            new_file(libc::fdopen(2, c"w".as_ptr())) as usize,
        ]
    }
});

extern "win64" fn __acrt_iob_func(index: u32) -> *mut File {
    STANDARD.get(index as usize).copied().unwrap_or(0) as *mut File
}

static COMMODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

extern "win64" fn __p__commode() -> *mut i32 {
    COMMODE.as_ptr()
}

extern "win64" fn _set_fmode(_: i32) -> i32 {
    0
}

/// Writes formatted output to a stream: the number of bytes, or -1.
fn write_out(file: *mut File, bytes: &[u8]) -> i32 {
    // SAFETY: a stream of ours and our bytes.
    let n = unsafe { libc::fwrite(bytes.as_ptr().cast(), 1, bytes.len(), inner(file)) };
    if n == bytes.len() { n as i32 } else { -1 }
}

extern "win64" fn __stdio_common_vfprintf(
    options: u64,
    file: *mut File,
    format: *const u8,
    _locale: *const c_void,
    args: *const u64,
) -> i32 {
    // SAFETY: the title's format and arguments.
    let out = unsafe { printf::format(format, &mut Args(args), options & printf::LEGACY_WIDE_SPECIFIERS != 0) };
    write_out(file, &out)
}

extern "win64" fn __stdio_common_vfwprintf(
    options: u64,
    file: *mut File,
    format: *const u16,
    _locale: *const c_void,
    args: *const u64,
) -> i32 {
    // SAFETY: as above.
    let out = unsafe { printf::format(format, &mut Args(args), options & printf::LEGACY_WIDE_SPECIFIERS != 0) };
    let text = String::from_utf16_lossy(&out);
    write_out(file, text.as_bytes()).min(out.len() as i32)
}

/// `_CRT_INTERNAL_PRINTF_STANDARD_SNPRINTF_BEHAVIOR`: C99 `snprintf`.
const STANDARD_SNPRINTF: u64 = 1 << 1;

/// Copies `out` and a NUL into `buffer` of `count` units by the options'
/// rules for `vsprintf`/`vsnprintf`.
fn put_counted<C: Unit>(options: u64, buffer: *mut C, count: usize, out: &[C]) -> i32 {
    let n = out.len();
    if buffer.is_null() {
        return n as i32;
    }
    // SAFETY: the title's buffer of `count` units.
    unsafe {
        if options & STANDARD_SNPRINTF != 0 {
            if count > 0 {
                let len = n.min(count - 1);
                std::ptr::copy_nonoverlapping(out.as_ptr(), buffer, len);
                buffer.add(len).write(C::default());
            }
            n as i32
        } else if n < count {
            std::ptr::copy_nonoverlapping(out.as_ptr(), buffer, n);
            buffer.add(n).write(C::default());
            n as i32
        } else {
            std::ptr::copy_nonoverlapping(out.as_ptr(), buffer, count);
            if n == count { n as i32 } else { -1 }
        }
    }
}

/// `_s` rules: the output and its NUL must fit in `size` units, else the
/// buffer is emptied and -1 returned. `max` (`_TRUNCATE` = all ones)
/// bounds the copied units.
fn put_secure<C: Unit>(buffer: *mut C, size: usize, max: usize, out: &[C]) -> i32 {
    if buffer.is_null() || size == 0 {
        set_errno(22);
        return -1;
    }
    let truncate = max == usize::MAX;
    let len = out.len().min(max);
    // SAFETY: the title's buffer of `size` units.
    unsafe {
        if len < size {
            std::ptr::copy_nonoverlapping(out.as_ptr(), buffer, len);
            buffer.add(len).write(C::default());
            if len < out.len() { -1 } else { len as i32 }
        } else if truncate {
            std::ptr::copy_nonoverlapping(out.as_ptr(), buffer, size - 1);
            buffer.add(size - 1).write(C::default());
            -1
        } else {
            buffer.write(C::default());
            set_errno(34);
            -1
        }
    }
}

macro_rules! sprintf {
    ($name:ident, $unit:ty, counted) => {
        extern "win64" fn $name(
            options: u64,
            buffer: *mut $unit,
            count: usize,
            format: *const $unit,
            _locale: *const c_void,
            args: *const u64,
        ) -> i32 {
            // SAFETY: the title's format and arguments.
            let out = unsafe { printf::format(format, &mut Args(args), options & printf::LEGACY_WIDE_SPECIFIERS != 0) };
            put_counted(options, buffer, count, &out)
        }
    };
    ($name:ident, $unit:ty, secure) => {
        extern "win64" fn $name(
            options: u64,
            buffer: *mut $unit,
            size: usize,
            format: *const $unit,
            _locale: *const c_void,
            args: *const u64,
        ) -> i32 {
            // SAFETY: as above.
            let out = unsafe { printf::format(format, &mut Args(args), options & printf::LEGACY_WIDE_SPECIFIERS != 0) };
            put_secure(buffer, size, out.len(), &out)
        }
    };
    ($name:ident, $unit:ty, secure_counted) => {
        extern "win64" fn $name(
            options: u64,
            buffer: *mut $unit,
            size: usize,
            max: usize,
            format: *const $unit,
            _locale: *const c_void,
            args: *const u64,
        ) -> i32 {
            // SAFETY: as above.
            let out = unsafe { printf::format(format, &mut Args(args), options & printf::LEGACY_WIDE_SPECIFIERS != 0) };
            put_secure(buffer, size, max, &out)
        }
    };
}

sprintf!(__stdio_common_vsprintf, u8, counted);
sprintf!(__stdio_common_vswprintf, u16, counted);
sprintf!(__stdio_common_vsprintf_s, u8, secure);
sprintf!(__stdio_common_vswprintf_s, u16, secure);
sprintf!(__stdio_common_vsnprintf_s, u8, secure_counted);
sprintf!(__stdio_common_vsnwprintf_s, u16, secure_counted);

extern "win64" fn __stdio_common_vsscanf(
    options: u64,
    input: *const u8,
    count: usize,
    format: *const u8,
    _locale: *const c_void,
    args: *const u64,
) -> i32 {
    // SAFETY: the title's strings and pointers.
    unsafe { scanf::scan(input, count, format, &mut Args(args), options) }
}

extern "win64" fn __stdio_common_vswscanf(
    options: u64,
    input: *const u16,
    count: usize,
    format: *const u16,
    _locale: *const c_void,
    args: *const u64,
) -> i32 {
    // SAFETY: as above.
    unsafe { scanf::scan(input, count, format, &mut Args(args), options) }
}

/// A Windows `fopen` mode as libc's: `r`, `w`, `a`, `+` and `x` kept; text,
/// binary, caching hints and `ccs=` dropped.
fn mode(windows: &str) -> CString {
    let main = windows.split(',').next().unwrap_or("");
    let kept: String = main.chars().filter(|c| "rwa+x".contains(*c)).collect();
    CString::new(kept).unwrap_or_default()
}

fn open_stream(path: &str, mode_text: &str) -> *mut File {
    let host = host_path(path);
    let Ok(c_path) = CString::new(host.as_os_str().as_encoded_bytes()) else {
        set_errno(22);
        return std::ptr::null_mut();
    };
    // SAFETY: NUL-terminated strings of ours.
    let file = new_file(unsafe { libc::fopen(c_path.as_ptr(), mode(mode_text).as_ptr()) });
    tracing::debug!(path, host = %host.display(), mode = mode_text, opened = !file.is_null(), "fopen");
    file
}

extern "win64" fn fopen(path: *const c_char, mode: *const c_char) -> *mut File {
    // SAFETY: the title's strings.
    unsafe { open_stream(&narrow(path), &narrow(mode)) }
}

extern "win64" fn _fsopen(path: *const c_char, mode: *const c_char, _share: i32) -> *mut File {
    fopen(path, mode)
}

extern "win64" fn _wfopen(path: *const u16, mode: *const u16) -> *mut File {
    // SAFETY: the title's strings.
    unsafe { open_stream(&wide(path), &wide(mode)) }
}

extern "win64" fn _wfsopen(path: *const u16, mode: *const u16, _share: i32) -> *mut File {
    _wfopen(path, mode)
}

extern "win64" fn fclose(file: *mut File) -> i32 {
    if file.is_null() {
        set_errno(22);
        return -1;
    }
    // SAFETY: a stream of ours; the standard ones stay.
    unsafe {
        let code = libc::fclose(inner(file));
        if !STANDARD.contains(&(file as usize)) {
            drop(Box::from_raw(file));
        }
        code
    }
}

extern "win64" fn fread(to: *mut c_void, size: usize, count: usize, file: *mut File) -> usize {
    // SAFETY: the title's buffer and our stream.
    unsafe { libc::fread(to, size, count, inner(file)) }
}

extern "win64" fn fwrite(from: *const c_void, size: usize, count: usize, file: *mut File) -> usize {
    // SAFETY: as above.
    unsafe { libc::fwrite(from, size, count, inner(file)) }
}

extern "win64" fn fgetc(file: *mut File) -> i32 {
    // SAFETY: our stream.
    unsafe { libc::fgetc(inner(file)) }
}

extern "win64" fn fputc(c: i32, file: *mut File) -> i32 {
    // SAFETY: our stream.
    unsafe { libc::fputc(c, inner(file)) }
}

extern "win64" fn fputs(text: *const c_char, file: *mut File) -> i32 {
    // SAFETY: the title's string and our stream.
    unsafe { libc::fputs(text, inner(file)) }
}

extern "win64" fn ungetc(c: i32, file: *mut File) -> i32 {
    // SAFETY: our stream.
    unsafe { libc::ungetc(c, inner(file)) }
}

const WEOF: u32 = 0xffff;

/// ponytail: wide characters are read and written as UTF-8 bytes, one
/// unit at a time for ASCII.
extern "win64" fn fgetwc(file: *mut File) -> u32 {
    match fgetc(file) {
        -1 => WEOF,
        b => b as u32,
    }
}

extern "win64" fn fputwc(c: u16, file: *mut File) -> u32 {
    let text: String = char::decode_utf16([c]).map(|r| r.unwrap_or('\u{fffd}')).collect();
    if write_out(file, text.as_bytes()) < 0 { WEOF } else { u32::from(c) }
}

extern "win64" fn ungetwc(c: u32, file: *mut File) -> u32 {
    if c < 0x80 && ungetc(c as i32, file) != -1 { c } else { WEOF }
}

extern "win64" fn fflush(file: *mut File) -> i32 {
    // SAFETY: our stream, or null for all.
    unsafe { libc::fflush(if file.is_null() { std::ptr::null_mut() } else { inner(file) }) }
}

extern "win64" fn ferror(file: *mut File) -> i32 {
    // SAFETY: our stream.
    unsafe { libc::ferror(inner(file)) }
}

extern "win64" fn fseek(file: *mut File, offset: i32, origin: i32) -> i32 {
    _fseeki64(file, i64::from(offset), origin)
}

extern "win64" fn _fseeki64(file: *mut File, offset: i64, origin: i32) -> i32 {
    // SAFETY: our stream; SEEK_* values are the same.
    unsafe { libc::fseeko(inner(file), offset, origin) }
}

extern "win64" fn ftell(file: *mut File) -> i32 {
    let at = _ftelli64(file);
    if at > i64::from(i32::MAX) {
        set_errno(22);
        -1
    } else {
        at as i32
    }
}

extern "win64" fn _ftelli64(file: *mut File) -> i64 {
    // SAFETY: our stream.
    unsafe { libc::ftello(inner(file)) }
}

extern "win64" fn fgetpos(file: *mut File, pos: *mut i64) -> i32 {
    let at = _ftelli64(file);
    if at < 0 {
        return -1;
    }
    // SAFETY: the title's `fpos_t` (64 bits).
    unsafe { pos.write(at) };
    0
}

extern "win64" fn fsetpos(file: *mut File, pos: *const i64) -> i32 {
    // SAFETY: as above.
    _fseeki64(file, unsafe { pos.read() }, libc::SEEK_SET)
}

extern "win64" fn rewind(file: *mut File) {
    // SAFETY: our stream.
    unsafe { libc::rewind(inner(file)) }
}

/// Windows' `_IOFBF`, `_IOLBF`, `_IONBF` are 0, 0x40, 4. The title's
/// buffer is not used: libc keeps its own, which outlives any of the title's.
extern "win64" fn setvbuf(file: *mut File, _buffer: *mut c_char, mode: i32, size: usize) -> i32 {
    let mode = match mode {
        0x40 => libc::_IOLBF,
        4 => libc::_IONBF,
        _ => libc::_IOFBF,
    };
    // SAFETY: our stream.
    unsafe { libc::setvbuf(inner(file), std::ptr::null_mut(), mode, size) }
}

extern "win64" fn _get_stream_buffer_pointers(
    file: *mut File,
    base: *mut *mut *mut u8,
    ptr: *mut *mut *mut u8,
    count: *mut *mut i32,
) -> i32 {
    // SAFETY: our stream and the caller's out pointers.
    unsafe {
        if !base.is_null() {
            base.write(&raw mut (*file).base);
        }
        if !ptr.is_null() {
            ptr.write(&raw mut (*file).ptr);
        }
        if !count.is_null() {
            count.write(&raw mut (*file).count);
        }
    }
    0
}

unsafe extern "C" {
    fn flockfile(file: *mut libc::FILE);
    fn funlockfile(file: *mut libc::FILE);
}

extern "win64" fn _lock_file(file: *mut File) {
    // SAFETY: our stream.
    unsafe { flockfile(inner(file)) }
}

extern "win64" fn _unlock_file(file: *mut File) {
    // SAFETY: our stream.
    unsafe { funlockfile(inner(file)) }
}

/// Windows `_O_*` flags as libc's.
fn open_flags(windows: i32) -> i32 {
    let mut flags = match windows & 3 {
        1 => libc::O_WRONLY,
        2 => libc::O_RDWR,
        _ => libc::O_RDONLY,
    };
    for (bit, flag) in [(0x8, libc::O_APPEND), (0x100, libc::O_CREAT), (0x200, libc::O_TRUNC), (0x400, libc::O_EXCL)] {
        if windows & bit != 0 {
            flags |= flag;
        }
    }
    flags | libc::O_CLOEXEC
}

fn open_fd(path: &str, flags: i32) -> i32 {
    let Ok(c_path) = CString::new(host_path(path).as_os_str().as_encoded_bytes()) else {
        set_errno(22);
        return -1;
    };
    // SAFETY: a NUL-terminated path of ours.
    let fd = unsafe { libc::open(c_path.as_ptr(), open_flags(flags), 0o644 as libc::c_uint) };
    if fd < 0 {
        set_errno(std::io::Error::last_os_error().raw_os_error().unwrap_or(0));
    }
    fd
}

extern "win64" fn _open(path: *const c_char, flags: i32, _mode: i32) -> i32 {
    // SAFETY: the title's string.
    open_fd(&unsafe { narrow(path) }, flags)
}

extern "win64" fn _wopen(path: *const u16, flags: i32, _mode: i32) -> i32 {
    // SAFETY: the title's string.
    open_fd(&unsafe { wide(path) }, flags)
}

extern "win64" fn _read(fd: i32, to: *mut c_void, count: u32) -> i32 {
    // SAFETY: the title's buffer.
    unsafe { libc::read(fd, to, count as usize) as i32 }
}

extern "win64" fn _write(fd: i32, from: *const c_void, count: u32) -> i32 {
    // SAFETY: the title's buffer.
    unsafe { libc::write(fd, from, count as usize) as i32 }
}

extern "win64" fn _close(fd: i32) -> i32 {
    // SAFETY: a descriptor the title opened.
    unsafe { libc::close(fd) }
}

extern "win64" fn _lseeki64(fd: i32, offset: i64, origin: i32) -> i64 {
    // SAFETY: as above.
    unsafe { libc::lseek(fd, offset, origin) }
}

/// The Universal C runtime's `struct _stat64`.
#[repr(C)]
struct Stat64 {
    dev: u32,
    ino: u16,
    mode: u16,
    nlink: i16,
    uid: i16,
    gid: i16,
    rdev: u32,
    size: i64,
    atime: i64,
    mtime: i64,
    ctime: i64,
}

fn with_path(path: *const u16, f: impl FnOnce(&std::path::Path) -> std::io::Result<()>) -> i32 {
    // SAFETY: the title's string.
    match f(&host_path(&unsafe { wide(path) })) {
        Ok(()) => 0,
        Err(e) => {
            set_errno(e.raw_os_error().unwrap_or(2));
            -1
        }
    }
}

extern "win64" fn _wstat64(path: *const u16, out: *mut Stat64) -> i32 {
    use std::os::unix::fs::MetadataExt;
    with_path(path, |p| {
        let m = std::fs::metadata(p)?;
        let kind = if m.is_dir() { 0x4000 } else { 0x8000 };
        let user = (m.mode() & 0o700) as u16;
        let rights = user | user >> 3 | user >> 6;
        // SAFETY: the title's out struct.
        unsafe {
            out.write(Stat64 {
                dev: 0,
                ino: 0,
                mode: kind | rights,
                nlink: 1,
                uid: 0,
                gid: 0,
                rdev: 0,
                size: m.len() as i64,
                atime: m.atime(),
                mtime: m.mtime(),
                ctime: m.ctime(),
            })
        };
        Ok(())
    })
}

extern "win64" fn _wremove(path: *const u16) -> i32 {
    with_path(path, |p| std::fs::remove_file(p))
}

extern "win64" fn remove(path: *const c_char) -> i32 {
    // SAFETY: the title's string.
    let path = host_path(&unsafe { narrow(path) });
    match std::fs::remove_file(path) {
        Ok(()) => 0,
        Err(e) => {
            set_errno(e.raw_os_error().unwrap_or(2));
            -1
        }
    }
}

extern "win64" fn _wrename(from: *const u16, to: *const u16) -> i32 {
    // SAFETY: the title's string.
    let to = host_path(&unsafe { wide(to) });
    with_path(from, |from| std::fs::rename(from, &to))
}

extern "win64" fn _wrmdir(path: *const u16) -> i32 {
    with_path(path, |p| std::fs::remove_dir(p))
}

extern "win64" fn _wchdir(path: *const u16) -> i32 {
    with_path(path, |p| std::env::set_current_dir(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprintf_buffers_follow_the_options() {
        let out = b"hello";
        let mut buf = [0xaau8; 8];
        assert_eq!(put_counted(STANDARD_SNPRINTF, buf.as_mut_ptr(), 4, out), 5, "C99: the full length");
        assert_eq!(&buf[..4], b"hel\0");
        assert_eq!(put_counted(0, buf.as_mut_ptr(), 4, out), -1, "legacy: too long");
        assert_eq!(put_counted(0, std::ptr::null_mut::<u8>(), 0, out), 5, "length query");
        assert_eq!(put_secure(buf.as_mut_ptr(), 8, usize::MAX, out), 5);
        assert_eq!(&buf[..6], b"hello\0");
        assert_eq!(put_secure(buf.as_mut_ptr(), 4, usize::MAX, out), -1, "_TRUNCATE");
        assert_eq!(&buf[..4], b"hel\0");
        assert_eq!(put_secure(buf.as_mut_ptr(), 4, 10, out), -1, "too small");
        assert_eq!(buf[0], 0, "emptied");
        assert_eq!(mode("rb, ccs=UTF-8").as_bytes(), b"r");
        assert_eq!(mode("w+t").as_bytes(), b"w+");
    }
}
