//! The crash log on macOS: lines also go to standard error.

use std::io::Write;

/// Appends `text` to `victory-crash.log` in the working folder.
pub fn log_crash(text: &str) {
    eprintln!("{text}");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("victory-crash.log")
    {
        let _ = writeln!(file, "{text}");
    }
}
