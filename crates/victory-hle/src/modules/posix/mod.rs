//! The Windows functions a title imports, on macOS: what the host's own
//! DLLs provide on Windows (`HostPolicy`). Registered after our other
//! modules, so a console-specific implementation there wins.

mod convert;
mod crt;
mod ctime;
mod files;
mod locale;
mod locks;
mod math;
mod misc;
mod net;
mod objects;
pub mod paths;
mod printf;
mod scanf;
mod seh;
pub use seh::install_signal_handlers;
mod stdio;
mod system;
mod wasapi;
mod string;

use crate::{HleExport, HleModule};

pub(crate) fn modules() -> Vec<HleModule> {
    let mut kernel: Vec<HleExport> = Vec::new();
    kernel.extend(misc::exports());
    kernel.extend(locks::exports());
    kernel.extend(objects::exports());
    kernel.extend(seh::exports());
    kernel.extend(files::exports());
    kernel.extend(system::exports());
    vec![
        HleModule {
            name: "posix kernel",
            dlls: &[
                "kernel32.dll",
                "kernelbase.dll",
                "ntdll.dll",
                "api-ms-win-core-*",
                "advapi32.dll",
                "bcrypt.dll",
                "ole32.dll",
                "user32.dll",
                "ext-ms-win-rtcore-ntuser-*",
            ],
            exports: kernel,
        },
        HleModule {
            name: "posix sockets",
            dlls: &["ws2_32.dll"],
            exports: net::exports(),
        },
        HleModule {
            name: "posix crt",
            dlls: &["api-ms-win-crt-*", "ucrtbase.dll"],
            exports: [crt::exports(), stdio::exports(), locale::exports(), string::exports(), math::exports(), ctime::exports(), convert::exports()].concat(),
        },
    ]
}

/// Kernel events for other modules, with the Windows signatures they use
/// there (`windows_sys`'s), so the same code builds on both.
#[allow(non_snake_case)]
pub(crate) mod events {
    use std::ffi::c_void;

    pub type HANDLE = *mut c_void;

    /// # Safety
    /// As Windows' (no pointer is read here).
    pub unsafe fn CreateEventW(_: *const c_void, manual: i32, initial: i32, _: *const u16) -> HANDLE {
        super::objects::CreateEventW(std::ptr::null(), manual, initial, std::ptr::null()) as HANDLE
    }

    /// # Safety
    /// As Windows'.
    pub unsafe fn SetEvent(event: HANDLE) -> i32 {
        super::objects::SetEvent(event as usize)
    }

    /// # Safety
    /// As Windows'.
    pub unsafe fn CloseHandle(handle: HANDLE) -> i32 {
        super::objects::CloseHandle(handle as usize)
    }
}

pub(crate) type Bool = i32;
pub(crate) const TRUE: Bool = 1;
pub(crate) const FALSE: Bool = 0;
pub(crate) const INFINITE: u32 = u32::MAX;

/// Windows error codes we set.
pub(crate) mod error {
    pub const INVALID_HANDLE: u32 = 6;
    pub const NOT_ENOUGH_MEMORY: u32 = 8;
    pub const INVALID_PARAMETER: u32 = 87;
    pub const NOT_OWNER: u32 = 288;
    pub const TIMEOUT: u32 = 1460;
}

/// The calling thread's block (`gs:[0x30]`), set up by the loader.
pub(crate) fn teb() -> usize {
    let teb: usize;
    // SAFETY: reads the pthread slot the loader filled for title threads.
    unsafe { std::arch::asm!("mov {}, gs:[0x30]", out(reg) teb, options(nostack, readonly)) };
    teb
}

/// A field of the calling thread's block, or 0 on a thread the loader
/// did not enter.
pub(crate) fn teb_field<T: Copy + Default>(offset: usize) -> T {
    match teb() {
        0 => T::default(),
        // SAFETY: inside the thread's block.
        teb => unsafe { ((teb + offset) as *const T).read() },
    }
}

const LAST_ERROR_AT: usize = 0x68;

thread_local! {
    /// The last error of a thread without a thread block (one of ours).
    static LAST_ERROR: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

pub fn set_last_error(code: u32) {
    match teb() {
        0 => LAST_ERROR.set(code),
        // SAFETY: inside the thread's block.
        teb => unsafe { ((teb + LAST_ERROR_AT) as *mut u32).write(code) },
    }
}

pub(crate) fn last_error() -> u32 {
    match teb() {
        0 => LAST_ERROR.get(),
        _ => teb_field(LAST_ERROR_AT),
    }
}

pub(crate) fn current_thread_id() -> u32 {
    match teb_field::<u32>(0x48) {
        0 => {
            let mut tid = 0u64;
            // SAFETY: a plain query about the calling thread.
            unsafe { libc::pthread_threadid_np(0, &mut tid) };
            victory_loader::thread_id(tid)
        }
        id => id,
    }
}

/// A timeout in milliseconds as a deadline (`None`: wait for ever).
pub(crate) fn deadline(ms: u32) -> Option<std::time::Instant> {
    (ms != INFINITE)
        .then(|| std::time::Instant::now() + std::time::Duration::from_millis(u64::from(ms)))
}
