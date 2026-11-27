//! The macOS display and GPU side (`mac/README.md`, step 6).
//!
//! [`run_app`] takes the main thread for AppKit: a window whose layer is a
//! `CAMetalLayer`, and the event loop. [`present`] may be called from any
//! thread (the title presents from its own): it draws into the layer's next
//! drawable and shows it.
//!
//! For now a present clears the window to a colour that changes with the
//! frame: the window, the device and the frame pacing work before the
//! title's shaders do.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod pads;

#[cfg(target_os = "macos")]
pub use pads::{Pad, pad};

#[cfg(target_os = "macos")]
pub use mac::{keys_held, present, run_app};

/// No window, no keys.
#[cfg(not(target_os = "macos"))]
pub fn keys_held() -> Vec<u8> {
    Vec::new()
}

/// Whether the title gets a window (`VICTORY_WINDOW=0`: none, for runs
/// without a display).
pub fn wanted() -> bool {
    std::env::var_os("VICTORY_WINDOW").is_none_or(|v| v != "0")
}
