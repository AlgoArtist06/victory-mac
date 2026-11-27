//! Windows paths as macOS paths. The console's drives are folders here
//! (`G:` the package, `T:` temporary files); `Z:` is the Mac's own root,
//! so a host path given to the title as `Z:\Users\...` comes back.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

static MOUNTS: Mutex<[Option<PathBuf>; 26]> = Mutex::new([const { None }; 26]);

/// Shows `dir` to the title as drive `letter`.
pub fn mount_at(letter: char, dir: &Path) {
    let index = (letter.to_ascii_uppercase() as u8).wrapping_sub(b'A') as usize;
    if let Some(slot) = MOUNTS.lock().unwrap_or_else(|e| e.into_inner()).get_mut(index) {
        *slot = Some(dir.to_owned());
    }
}

/// The macOS path for a Windows one: `\\?\` dropped, the drive replaced by
/// its folder, backslashes made slashes. A path without a drive is left
/// relative (to the process's folder).
pub fn host_path(windows: &str) -> PathBuf {
    let path = windows
        .strip_prefix(r"\\?\")
        .or_else(|| windows.strip_prefix(r"\??\"))
        .unwrap_or(windows);
    let rest = path.replace('\\', "/");
    let bytes = rest.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        let letter = bytes[0].to_ascii_uppercase();
        let tail = rest[2..].trim_start_matches('/');
        let mounted = MOUNTS.lock().unwrap_or_else(|e| e.into_inner())[(letter - b'A') as usize].clone();
        let root = match mounted {
            Some(dir) => dir,
            None if letter == b'Z' => PathBuf::from("/"),
            None => {
                tracing::warn!(path = windows, "a path on a drive we do not have");
                PathBuf::from(format!("/nonexistent-drive-{}", letter as char))
            }
        };
        return if tail.is_empty() { root } else { root.join(tail) };
    }
    PathBuf::from(rest)
}

/// The Windows path the title sees for a macOS one: under a mounted
/// drive if one holds it, else on `Z:`.
pub fn windows_path(host: &Path) -> String {
    let mounts = MOUNTS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for (i, dir) in mounts.iter().enumerate() {
        if let Some(rest) = dir.as_ref().and_then(|d| host.strip_prefix(d).ok()) {
            let letter = (b'A' + i as u8) as char;
            return format!(r"{letter}:\{}", rest.to_string_lossy().replace('/', "\\"));
        }
    }
    format!("Z:{}", host.to_string_lossy().replace('/', "\\"))
}

/// A NUL-terminated UTF-16 string from the title.
///
/// # Safety
/// `p` points at a NUL-terminated wide string, or is null.
pub unsafe fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: guaranteed by the caller.
    let len = (0..).take_while(|&i| unsafe { *p.add(i) } != 0).count();
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

/// A NUL-terminated narrow string from the title (UTF-8, lossy).
///
/// # Safety
/// `p` points at a NUL-terminated string, or is null.
pub unsafe fn narrow(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: guaranteed by the caller.
    unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_map_to_folders_and_back() {
        mount_at('G', Path::new("/games/title"));
        assert_eq!(host_path(r"G:\data\x.rpf"), Path::new("/games/title/data/x.rpf"));
        assert_eq!(host_path(r"g:/data"), Path::new("/games/title/data"));
        assert_eq!(host_path(r"\\?\G:\a"), Path::new("/games/title/a"));
        assert_eq!(host_path(r"Z:\Users\me"), Path::new("/Users/me"));
        assert_eq!(host_path(r"rel\file"), Path::new("rel/file"));
        assert_eq!(windows_path(Path::new("/games/title/a/b")), r"G:\a\b");
        assert_eq!(windows_path(Path::new("/Users/me")), r"Z:\Users\me");
    }
}
