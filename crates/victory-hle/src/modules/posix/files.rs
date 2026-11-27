//! Files, folders and searches (`api-ms-win-core-file-*`), on the Mac's
//! files through the drive map ([`super::paths`]).
#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::objects::{insert_other, other};
use super::paths::{host_path, narrow, wide};
use super::{Bool, FALSE, TRUE, set_last_error};
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(CreateFileW),
        hle_export!(CreateFileA),
        hle_export!(CreateFile2),
        hle_export!(ReadFile),
        hle_export!(WriteFile),
        hle_export!(SetFilePointer),
        hle_export!(SetFilePointerEx),
        hle_export!(GetFileSizeEx),
        hle_export!(GetFileInformationByHandle),
        hle_export!(GetFileTime),
        hle_export!(SetFileTime),
        hle_export!(SetEndOfFile),
        hle_export!(FlushFileBuffers),
        hle_export!(GetFileAttributesW),
        hle_export!(GetFileAttributesA),
        hle_export!(GetFileAttributesExW),
        hle_export!(SetFileAttributesW),
        hle_export!(CreateDirectoryW),
        hle_export!(DeleteFileW),
        hle_export!(RemoveDirectoryW),
        hle_export!(MoveFileExW),
        hle_export!(CopyFile2),
        hle_export!(CreateHardLinkW),
        hle_export!(CreateSymbolicLinkW),
        hle_export!(FindFirstFileW),
        hle_export!(FindFirstFileExW),
        hle_export!(FindNextFileW),
        hle_export!(FindClose),
        hle_export!(GetDiskFreeSpaceExW),
        hle_export!(GetTempPathW),
    ]
}

const INVALID_HANDLE_VALUE: usize = usize::MAX;
const INVALID_FILE_ATTRIBUTES: u32 = u32::MAX;

/// The Windows error for an I/O error.
fn win_error(e: &std::io::Error) -> u32 {
    match e.raw_os_error() {
        Some(libc::ENOENT) => 2,                // ERROR_FILE_NOT_FOUND
        Some(libc::ENOTDIR) => 3,               // ERROR_PATH_NOT_FOUND
        Some(libc::EACCES | libc::EPERM | libc::EISDIR) => 5, // ERROR_ACCESS_DENIED
        Some(libc::EEXIST) => 80,               // ERROR_FILE_EXISTS
        Some(libc::ENOSPC) => 112,              // ERROR_DISK_FULL
        Some(libc::ENOTEMPTY) => 145,           // ERROR_DIR_NOT_EMPTY
        Some(libc::EBADF) => 6,                 // ERROR_INVALID_HANDLE
        _ => match e.kind() {
            ErrorKind::NotFound => 2,
            ErrorKind::PermissionDenied => 5,
            ErrorKind::AlreadyExists => 80,
            _ => 31, // ERROR_GEN_FAILURE
        },
    }
}

fn fail<T>(e: &std::io::Error, value: T) -> T {
    set_last_error(win_error(e));
    value
}

/// An open file: the Mac's file and the Windows file pointer.
struct OpenFile {
    file: File,
    path: PathBuf,
    position: Mutex<u64>,
}

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const FILE_WRITE_DATA: u32 = 0x2;
const FILE_APPEND_DATA: u32 = 0x4;

fn open(path: &str, access: u32, disposition: u32) -> usize {
    let host = host_path(path);
    let write = access & (GENERIC_WRITE | GENERIC_ALL | FILE_WRITE_DATA | FILE_APPEND_DATA) != 0;
    let existed = host.exists();
    let mut options = OpenOptions::new();
    options.read(true).write(write).custom_flags(libc::O_CLOEXEC);
    match disposition {
        1 => options.create_new(true),                 // CREATE_NEW
        2 => options.create(true).truncate(true),      // CREATE_ALWAYS
        4 => options.create(true),                     // OPEN_ALWAYS
        5 => options.truncate(true),                   // TRUNCATE_EXISTING
        _ => &mut options,                             // OPEN_EXISTING
    };
    let opened = options.open(&host).or_else(|e| {
        // A folder (opened for its information) or a read-only file.
        if write || !host.is_dir() { Err(e) } else { File::open(&host) }
    });
    match opened {
        Ok(file) => {
            tracing::trace!(path, host = %host.display(), "file opened");
            // ERROR_ALREADY_EXISTS tells CREATE_ALWAYS/OPEN_ALWAYS callers
            // the file was there.
            set_last_error(if existed && matches!(disposition, 2 | 4) { 183 } else { 0 });
            insert_other(Arc::new(OpenFile {
                file,
                path: host,
                position: Mutex::new(0),
            }))
        }
        Err(e) => {
            tracing::debug!(path, host = %host.display(), error = %e, "file not opened");
            fail(&e, INVALID_HANDLE_VALUE)
        }
    }
}

extern "win64" fn CreateFileW(path: *const u16, access: u32, _share: u32, _: *const c_void, disposition: u32, _flags: u32, _: usize) -> usize {
    // SAFETY: the title's string.
    open(&unsafe { wide(path) }, access, disposition)
}

extern "win64" fn CreateFileA(path: *const c_char, access: u32, _share: u32, _: *const c_void, disposition: u32, _flags: u32, _: usize) -> usize {
    // SAFETY: the title's string.
    open(&unsafe { narrow(path) }, access, disposition)
}

extern "win64" fn CreateFile2(path: *const u16, access: u32, _share: u32, disposition: u32, _: *const c_void) -> usize {
    // SAFETY: the title's string.
    open(&unsafe { wide(path) }, access, disposition)
}

/// `OVERLAPPED`: status, bytes, offset (low, high), event.
#[repr(C)]
struct Overlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: usize,
}

fn finish_overlapped(overlapped: *mut Overlapped, bytes: usize) {
    if overlapped.is_null() {
        return;
    }
    // SAFETY: the caller's OVERLAPPED.
    unsafe {
        (*overlapped).internal = 0;
        (*overlapped).internal_high = bytes;
        let event = (*overlapped).event & !1;
        if event != 0 {
            let set = crate::registry::lookup("kernel32.dll", "SetEvent").expect("ours");
            let set: extern "win64" fn(usize) -> Bool = std::mem::transmute(set);
            set(event);
        }
    }
}

/// Reads or writes at the overlapped offset, or at the file pointer
/// (which then moves on). ponytail: overlapped requests finish at once.
fn transfer(handle: usize, overlapped: *mut Overlapped, len: usize, done: *mut u32, op: impl FnOnce(&File, u64) -> std::io::Result<usize>) -> Bool {
    let Some(f) = other::<OpenFile>(handle) else { return FALSE };
    let mut position = f.position.lock().unwrap_or_else(|e| e.into_inner());
    let at = if overlapped.is_null() {
        *position
    } else {
        // SAFETY: the caller's OVERLAPPED.
        unsafe { u64::from((*overlapped).offset) | u64::from((*overlapped).offset_high) << 32 }
    };
    match op(&f.file, at) {
        Ok(n) => {
            *position = at + n as u64;
            if !done.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { done.write(n as u32) };
            }
            finish_overlapped(overlapped, n);
            let _ = len;
            TRUE
        }
        Err(e) => fail(&e, FALSE),
    }
}

/// Reads until `buf` is full or the file ends, as Windows does for files.
fn read_full(file: &File, buf: &mut [u8], at: u64) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match file.read_at(&mut buf[n..], at + n as u64) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

extern "win64" fn ReadFile(handle: usize, to: *mut u8, len: u32, done: *mut u32, overlapped: *mut Overlapped) -> Bool {
    transfer(handle, overlapped, len as usize, done, |file, at| {
        // SAFETY: the caller's buffer of `len` bytes.
        let buf = unsafe { std::slice::from_raw_parts_mut(to, len as usize) };
        read_full(file, buf, at)
    })
}

extern "win64" fn WriteFile(handle: usize, from: *const u8, len: u32, done: *mut u32, overlapped: *mut Overlapped) -> Bool {
    transfer(handle, overlapped, len as usize, done, |file, at| {
        // SAFETY: the caller's buffer of `len` bytes.
        let buf = unsafe { std::slice::from_raw_parts(from, len as usize) };
        file.write_all_at(buf, at).map(|()| buf.len())
    })
}

/// Moves the file pointer: from the start, the pointer or the end (0-2).
fn seek(handle: usize, distance: i64, method: u32) -> Option<u64> {
    let f = other::<OpenFile>(handle)?;
    let mut position = f.position.lock().unwrap_or_else(|e| e.into_inner());
    let from = match method {
        0 => 0,
        1 => *position as i64,
        _ => match f.file.metadata() {
            Ok(m) => m.len() as i64,
            Err(e) => return fail(&e, None),
        },
    };
    let to = from + distance;
    if to < 0 {
        set_last_error(131); // ERROR_NEGATIVE_SEEK
        return None;
    }
    *position = to as u64;
    Some(to as u64)
}

extern "win64" fn SetFilePointer(handle: usize, low: i32, high: *mut i32, method: u32) -> u32 {
    let distance = if high.is_null() {
        i64::from(low)
    } else {
        // SAFETY: the caller's high half.
        i64::from(unsafe { high.read() }) << 32 | i64::from(low as u32)
    };
    match seek(handle, distance, method) {
        Some(at) => {
            if !high.is_null() {
                // SAFETY: as above.
                unsafe { high.write((at >> 32) as i32) };
            }
            set_last_error(0);
            at as u32
        }
        None => u32::MAX,
    }
}

extern "win64" fn SetFilePointerEx(handle: usize, distance: i64, new: *mut u64, method: u32) -> Bool {
    match seek(handle, distance, method) {
        Some(at) => {
            if !new.is_null() {
                // SAFETY: the caller's out pointer.
                unsafe { new.write(at) };
            }
            TRUE
        }
        None => FALSE,
    }
}

extern "win64" fn GetFileSizeEx(handle: usize, size: *mut u64) -> Bool {
    let Some(f) = other::<OpenFile>(handle) else { return FALSE };
    match f.file.metadata() {
        Ok(m) => {
            // SAFETY: the caller's out pointer.
            unsafe { size.write(m.len()) };
            TRUE
        }
        Err(e) => fail(&e, FALSE),
    }
}

/// FILETIME of a Unix time in seconds and nanoseconds.
fn filetime(seconds: i64, nanos: i64) -> u64 {
    ((seconds + 11_644_473_600) * 10_000_000 + nanos / 100) as u64
}

fn attributes(m: &std::fs::Metadata) -> u32 {
    let mut a = if m.is_dir() { 0x10 } else { 0x20 };
    if m.permissions().readonly() {
        a |= 1;
    }
    a
}

/// `WIN32_FILE_ATTRIBUTE_DATA`'s fields: attributes, creation, last access
/// and last write times, size high and low.
fn attribute_data(m: &std::fs::Metadata) -> (u32, [u64; 3], u64) {
    let times = [
        filetime(m.ctime(), m.ctime_nsec()),
        filetime(m.atime(), m.atime_nsec()),
        filetime(m.mtime(), m.mtime_nsec()),
    ];
    (attributes(m), times, m.len())
}

#[repr(C, packed(4))]
struct HandleInformation {
    attributes: u32,
    times: [u64; 3],
    volume: u32,
    size_high: u32,
    size_low: u32,
    links: u32,
    index_high: u32,
    index_low: u32,
}

extern "win64" fn GetFileInformationByHandle(handle: usize, out: *mut HandleInformation) -> Bool {
    let Some(f) = other::<OpenFile>(handle) else { return FALSE };
    match f.file.metadata() {
        Ok(m) => {
            let (attributes, times, size) = attribute_data(&m);
            // SAFETY: the caller's struct.
            unsafe {
                out.write_unaligned(HandleInformation {
                    attributes,
                    times,
                    volume: m.dev() as u32,
                    size_high: (size >> 32) as u32,
                    size_low: size as u32,
                    links: m.nlink() as u32,
                    index_high: (m.ino() >> 32) as u32,
                    index_low: m.ino() as u32,
                })
            };
            TRUE
        }
        Err(e) => fail(&e, FALSE),
    }
}

extern "win64" fn GetFileTime(handle: usize, created: *mut u64, accessed: *mut u64, written: *mut u64) -> Bool {
    let Some(f) = other::<OpenFile>(handle) else { return FALSE };
    match f.file.metadata() {
        Ok(m) => {
            let (_, times, _) = attribute_data(&m);
            for (out, t) in [created, accessed, written].into_iter().zip(times) {
                if !out.is_null() {
                    // SAFETY: the caller's FILETIME.
                    unsafe { out.write_unaligned(t) };
                }
            }
            TRUE
        }
        Err(e) => fail(&e, FALSE),
    }
}

/// ponytail: file times are not set.
extern "win64" fn SetFileTime(_: usize, _: *const u64, _: *const u64, _: *const u64) -> Bool {
    TRUE
}

extern "win64" fn SetEndOfFile(handle: usize) -> Bool {
    let Some(f) = other::<OpenFile>(handle) else { return FALSE };
    let at = *f.position.lock().unwrap_or_else(|e| e.into_inner());
    match f.file.set_len(at) {
        Ok(()) => TRUE,
        Err(e) => fail(&e, FALSE),
    }
}

extern "win64" fn FlushFileBuffers(handle: usize) -> Bool {
    let Some(f) = other::<OpenFile>(handle) else { return FALSE };
    match f.file.sync_data() {
        Ok(()) => TRUE,
        Err(e) => fail(&e, FALSE),
    }
}

fn file_attributes(path: &str) -> u32 {
    match std::fs::metadata(host_path(path)) {
        Ok(m) => attributes(&m),
        Err(e) => fail(&e, INVALID_FILE_ATTRIBUTES),
    }
}

extern "win64" fn GetFileAttributesW(path: *const u16) -> u32 {
    // SAFETY: the title's string.
    file_attributes(&unsafe { wide(path) })
}

extern "win64" fn GetFileAttributesA(path: *const c_char) -> u32 {
    // SAFETY: the title's string.
    file_attributes(&unsafe { narrow(path) })
}

#[repr(C, packed(4))]
struct AttributeData {
    attributes: u32,
    times: [u64; 3],
    size_high: u32,
    size_low: u32,
}

extern "win64" fn GetFileAttributesExW(path: *const u16, _level: u32, out: *mut AttributeData) -> Bool {
    // SAFETY: the title's string.
    match std::fs::metadata(host_path(&unsafe { wide(path) })) {
        Ok(m) => {
            let (attributes, times, size) = attribute_data(&m);
            // SAFETY: the caller's struct.
            unsafe {
                out.write_unaligned(AttributeData {
                    attributes,
                    times,
                    size_high: (size >> 32) as u32,
                    size_low: size as u32,
                })
            };
            TRUE
        }
        Err(e) => fail(&e, FALSE),
    }
}

/// ponytail: attributes (hidden, read-only) are not kept.
extern "win64" fn SetFileAttributesW(path: *const u16, _: u32) -> Bool {
    // SAFETY: the title's string.
    if host_path(&unsafe { wide(path) }).exists() { TRUE } else { set_last_error(2); FALSE }
}

fn on_path(path: *const u16, f: impl FnOnce(&Path) -> std::io::Result<()>) -> Bool {
    // SAFETY: the title's string.
    match f(&host_path(&unsafe { wide(path) })) {
        Ok(()) => TRUE,
        Err(e) => fail(&e, FALSE),
    }
}

extern "win64" fn CreateDirectoryW(path: *const u16, _: *const c_void) -> Bool {
    let made = on_path(path, |p| std::fs::create_dir(p));
    if made == FALSE && super::last_error() == 80 {
        set_last_error(183); // ERROR_ALREADY_EXISTS
    }
    made
}

extern "win64" fn DeleteFileW(path: *const u16) -> Bool {
    on_path(path, |p| std::fs::remove_file(p))
}

extern "win64" fn RemoveDirectoryW(path: *const u16) -> Bool {
    on_path(path, |p| std::fs::remove_dir(p))
}

const MOVEFILE_REPLACE_EXISTING: u32 = 1;

extern "win64" fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> Bool {
    // SAFETY: the title's string.
    let to = host_path(&unsafe { wide(to) });
    on_path(from, |from| {
        if flags & MOVEFILE_REPLACE_EXISTING == 0 && to.exists() {
            return Err(std::io::Error::from_raw_os_error(libc::EEXIST));
        }
        std::fs::rename(from, &to)
    })
}

extern "win64" fn CopyFile2(from: *const u16, to: *const u16, _: *const c_void) -> i32 {
    // SAFETY: the title's string.
    let to = host_path(&unsafe { wide(to) });
    if on_path(from, |from| std::fs::copy(from, &to).map(|_| ())) == TRUE {
        0
    } else {
        (0x8007_0000 | super::last_error()) as i32
    }
}

extern "win64" fn CreateHardLinkW(link: *const u16, target: *const u16, _: *const c_void) -> Bool {
    // SAFETY: the title's string.
    let target = host_path(&unsafe { wide(target) });
    on_path(link, |link| std::fs::hard_link(&target, link))
}

extern "win64" fn CreateSymbolicLinkW(link: *const u16, target: *const u16, _: u32) -> u8 {
    // SAFETY: the title's string.
    let target = host_path(&unsafe { wide(target) });
    on_path(link, |link| std::os::unix::fs::symlink(&target, link)) as u8
}

/// A search: the matching names left, with their information.
struct Search {
    entries: Mutex<std::vec::IntoIter<(String, std::fs::Metadata)>>,
}

/// Windows wildcards (`*`, `?`), case ignored; `*.*` matches every name.
fn matches(pattern: &[char], name: &[char]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some(('*', rest)) => (0..=name.len()).any(|i| matches(rest, &name[i..])),
        Some(('?', rest)) => !name.is_empty() && matches(rest, &name[1..]),
        Some((c, rest)) => name.first().is_some_and(|n| n.eq_ignore_ascii_case(c)) && matches(rest, &name[1..]),
    }
}

#[repr(C, packed(4))]
struct FindData {
    attributes: u32,
    times: [u64; 3],
    size_high: u32,
    size_low: u32,
    reserved: [u32; 2],
    name: [u16; 260],
    short_name: [u16; 14],
}

fn fill(out: *mut FindData, name: &str, m: &std::fs::Metadata) {
    let (attributes, times, size) = attribute_data(m);
    let mut data = FindData {
        attributes,
        times,
        size_high: (size >> 32) as u32,
        size_low: size as u32,
        reserved: [0; 2],
        name: [0; 260],
        short_name: [0; 14],
    };
    for (slot, unit) in data.name.iter_mut().zip(name.encode_utf16().take(259)) {
        *slot = unit;
    }
    // SAFETY: the caller's struct.
    unsafe { out.write_unaligned(data) };
}

fn next(search: &Search, out: *mut FindData) -> bool {
    match search.entries.lock().unwrap_or_else(|e| e.into_inner()).next() {
        Some((name, m)) => {
            fill(out, &name, &m);
            true
        }
        None => false,
    }
}

const ERROR_NO_MORE_FILES: u32 = 18;

extern "win64" fn FindFirstFileW(pattern: *const u16, out: *mut FindData) -> usize {
    // SAFETY: the title's string.
    let pattern = unsafe { wide(pattern) };
    let (dir, glob) = match pattern.rfind(['\\', '/']) {
        Some(i) => (&pattern[..i], &pattern[i + 1..]),
        None => (".", pattern.as_str()),
    };
    let glob: Vec<char> = if glob == "*.*" { vec!['*'] } else { glob.chars().collect() };
    let listing = match std::fs::read_dir(host_path(dir)) {
        Ok(listing) => listing,
        Err(e) => return fail(&e, INVALID_HANDLE_VALUE),
    };
    let mut entries: Vec<(String, std::fs::Metadata)> = listing
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            let chars: Vec<char> = name.chars().collect();
            (matches(&glob, &chars) && !name.starts_with("._")).then_some((name, e.metadata().ok()?))
        })
        .collect();
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    let search = Search {
        entries: Mutex::new(entries.into_iter()),
    };
    if !next(&search, out) {
        set_last_error(2);
        return INVALID_HANDLE_VALUE;
    }
    insert_other(Arc::new(search))
}

extern "win64" fn FindFirstFileExW(pattern: *const u16, _level: u32, out: *mut FindData, _op: u32, _: *const c_void, _flags: u32) -> usize {
    FindFirstFileW(pattern, out)
}

extern "win64" fn FindNextFileW(handle: usize, out: *mut FindData) -> Bool {
    let Some(search) = other::<Search>(handle) else { return FALSE };
    if next(&search, out) {
        TRUE
    } else {
        set_last_error(ERROR_NO_MORE_FILES);
        FALSE
    }
}

extern "win64" fn FindClose(handle: usize) -> Bool {
    let close = crate::registry::lookup("kernel32.dll", "CloseHandle").expect("ours");
    // SAFETY: our own CloseHandle.
    let close: extern "win64" fn(usize) -> Bool = unsafe { std::mem::transmute(close) };
    close(handle)
}

extern "win64" fn GetDiskFreeSpaceExW(path: *const u16, caller: *mut u64, total: *mut u64, free: *mut u64) -> Bool {
    // SAFETY: the title's string.
    let host = if path.is_null() { PathBuf::from(".") } else { host_path(&unsafe { wide(path) }) };
    let Ok(c_path) = std::ffi::CString::new(host.as_os_str().as_encoded_bytes()) else { return FALSE };
    // SAFETY: a NUL-terminated path and our struct.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut st) } != 0 {
        return fail(&std::io::Error::last_os_error(), FALSE);
    }
    let block = u64::from(st.f_bsize);
    for (out, value) in [(caller, st.f_bavail * block), (total, st.f_blocks * block), (free, st.f_bfree * block)] {
        if !out.is_null() {
            // SAFETY: the caller's out pointer.
            unsafe { out.write(value) };
        }
    }
    TRUE
}

/// The temporary folder is the console's `T:` drive.
extern "win64" fn GetTempPathW(len: u32, out: *mut u16) -> u32 {
    let path: Vec<u16> = "T:\\".encode_utf16().collect();
    if (len as usize) <= path.len() {
        return path.len() as u32 + 1;
    }
    // SAFETY: the caller's buffer of `len` units.
    unsafe {
        std::ptr::copy_nonoverlapping(path.as_ptr(), out, path.len());
        out.add(path.len()).write(0);
    }
    path.len() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_and_searches_behave_as_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        super::super::paths::mount_at('Q', dir.path());
        let w = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let h = CreateFileW(w(r"Q:\a.bin").as_ptr(), GENERIC_WRITE, 0, std::ptr::null(), 2, 0, 0);
        assert_ne!(h, INVALID_HANDLE_VALUE);
        let mut n = 0u32;
        assert_eq!(WriteFile(h, b"hello world".as_ptr(), 11, &mut n, std::ptr::null_mut()), TRUE);
        assert_eq!(n, 11);
        let mut at = 0u64;
        assert_eq!(SetFilePointerEx(h, -5, &mut at, 2), TRUE);
        assert_eq!(at, 6, "5 back from the end");
        let mut buf = [0u8; 16];
        assert_eq!(ReadFile(h, buf.as_mut_ptr(), 16, &mut n, std::ptr::null_mut()), TRUE);
        assert_eq!(&buf[..n as usize], b"world");
        let mut ov = Overlapped { internal: 9, internal_high: 0, offset: 0, offset_high: 0, event: 0 };
        assert_eq!(ReadFile(h, buf.as_mut_ptr(), 5, std::ptr::null_mut(), &mut ov), TRUE);
        assert_eq!((&buf[..5], ov.internal, ov.internal_high), (&b"hello"[..], 0, 5));
        let mut size = 0u64;
        assert_eq!(GetFileSizeEx(h, &mut size), TRUE);
        assert_eq!(size, 11);
        assert_eq!(CreateFileW(w(r"Q:\missing").as_ptr(), GENERIC_READ, 0, std::ptr::null(), 3, 0, 0), INVALID_HANDLE_VALUE);
        assert_eq!(super::super::last_error(), 2);
        assert_eq!(CreateDirectoryW(w(r"Q:\sub").as_ptr(), std::ptr::null()), TRUE);
        assert_eq!(GetFileAttributesW(w(r"Q:\sub").as_ptr()) & 0x10, 0x10);
        let mut data: FindData = unsafe { std::mem::zeroed() };
        let search = FindFirstFileW(w(r"Q:\*.BIN").as_ptr(), &mut data);
        assert_ne!(search, INVALID_HANDLE_VALUE, "case ignored");
        let name = data.name;
        assert_eq!(String::from_utf16_lossy(&name[..5]), "a.bin");
        assert_eq!(FindNextFileW(search, &mut data), FALSE);
        assert_eq!(super::super::last_error(), ERROR_NO_MORE_FILES);
        assert!(matches(&['*'], &['x']));
        assert!(!matches(&"a?c".chars().collect::<Vec<_>>(), &['a', 'c']));
    }
}
