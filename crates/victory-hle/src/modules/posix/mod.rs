//! The Windows functions a title imports, on macOS: what the host's own
//! DLLs provide on Windows (`HostPolicy`). Registered after our other
//! modules, so a console-specific implementation there wins.

mod crt;
mod locale;
mod locks;
mod math;
mod misc;
mod objects;
pub mod paths;
mod printf;
mod scanf;
mod stdio;
mod string;

use crate::{HleExport, HleModule};

pub(crate) fn modules() -> Vec<HleModule> {
    let mut kernel: Vec<HleExport> = Vec::new();
    kernel.extend(misc::exports());
    kernel.extend(locks::exports());
    kernel.extend(objects::exports());
    vec![
        HleModule {
            name: "posix kernel",
            dlls: &[
                "kernel32.dll",
                "kernelbase.dll",
                "ntdll.dll",
                "api-ms-win-core-*",
            ],
            exports: kernel,
        },
        HleModule {
            name: "posix crt",
            dlls: &["api-ms-win-crt-*", "ucrtbase.dll"],
            exports: [crt::exports(), stdio::exports(), locale::exports(), string::exports(), math::exports()].concat(),
        },
    ]
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

const LAST_ERROR: usize = 0x68;

pub(crate) fn set_last_error(code: u32) {
    let teb = teb();
    if teb != 0 {
        // SAFETY: inside the thread's block.
        unsafe { ((teb + LAST_ERROR) as *mut u32).write(code) };
    }
}

pub(crate) fn last_error() -> u32 {
    teb_field(LAST_ERROR)
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
