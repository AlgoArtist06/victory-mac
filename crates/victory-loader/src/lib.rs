//! Runs a title's original x86-64 code natively, inside our process.
//!
//! Xbox Series X|S executables are x86-64 PE32+ images built for a Zen 2
//! CPU, so the machine code needs no translation. What differs is the
//! platform underneath, so the loader's job is to:
//!
//! 1. [`prepare()`] a lightly patched copy of the image (pure, portable): mark
//!    it as a DLL, remove its entry point and import directory, and detach
//!    its TLS callbacks, so the Windows loader maps it, relocates it,
//!    allocates its static TLS and registers its unwind tables, but runs
//!    none of its code and resolves none of its imports;
//! 2. load that copy as a real module (debuggers, profilers, crash dumps
//!    and stack walks all see it); this step is Windows-only;
//! 3. map the DLLs the title ships beside it the same way
//!    (`load_title_dlls`);
//! 4. bind every import ourselves through an [`ImportResolver`], our
//!    runtime (`victory-hle`), the host or the title's DLLs, and route
//!    anything unresolved to a generated stub that reports exactly what
//!    was called;
//! 5. initialise the title's DLLs, then run TLS callbacks and the entry
//!    point on a thread with the stack size the image asks for; from then
//!    on, pass every thread start and end on to the attached images.

#[cfg(any(windows, target_os = "macos"))]
mod console_insn;
#[cfg(windows)]
mod crash;
#[cfg(target_os = "macos")]
mod crash_macos;
#[cfg(windows)]
pub mod descriptor_sites;
pub mod frame_rate;
#[cfg(any(windows, target_os = "macos"))]
mod images;
#[cfg(target_os = "macos")]
mod macos;
pub mod prepare;
#[cfg(all(windows, feature = "diagnostics"))]
mod profile;
#[cfg(windows)]
pub(crate) mod publish;
pub mod resolution;
pub mod resolve;
#[cfg(all(windows, feature = "diagnostics"))]
mod stacks;
#[cfg(windows)]
mod threads;
#[cfg(any(windows, target_os = "macos"))]
mod thunks;
#[cfg(windows)]
mod tls;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub mod write_sites;

#[cfg(all(windows, feature = "diagnostics"))]
pub use crash::save_on_crash;
#[cfg(windows)]
pub use crash::{
    locate, log_crash, log_crashes_to, note_title_output, report_crashes, report_unhandled,
};
#[cfg(target_os = "macos")]
pub use crash_macos::log_crash;

static PROBE_BASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Sets the first-hit probes `VICTORY_PROBE` names, offsets into the
/// executable mapped at `base` (a diagnostic; see `console_insn`).
#[cfg(any(windows, target_os = "macos"))]
pub fn set_probes(base: usize) {
    PROBE_BASE.store(base, std::sync::atomic::Ordering::Relaxed);
    console_insn::set_probes(base);
}

/// Whether the `int3` at `at` was a probe's (now gone; the thread may go on
/// at `at`).
#[cfg(any(windows, target_os = "macos"))]
pub fn probe_hit(at: usize) -> bool {
    console_insn::probe_hit(at)
}

#[cfg(any(windows, target_os = "macos"))]
fn probe_base() -> usize {
    PROBE_BASE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Emulates the console-only instruction a fault stopped at, if it is one,
/// and steps over it: true then. `context` is the fault's Windows `CONTEXT`.
///
/// # Safety
/// `context` is the faulting thread's context, being dispatched.
#[cfg(target_os = "macos")]
pub unsafe fn emulate_console_instruction(context: *mut std::ffi::c_void) -> bool {
    // SAFETY: the same layout, forwarded from the caller.
    unsafe { console_insn::emulate(&mut *context.cast::<console_insn::CONTEXT>()) == -1 }
}
#[cfg(any(windows, target_os = "macos"))]
pub use images::{BindReport, MissingImport, WithTitleDlls, load_title_dll, load_title_dlls};
#[cfg(target_os = "macos")]
pub use macos::{LoadedImage, enter_every_thread, enter_thread, enter_title_thread, export_of, image_containing, load, thread_id};
pub use prepare::{PrepareError, PreparedImage, prepare};
#[cfg(all(windows, feature = "diagnostics"))]
pub use profile::profile_on_schedule;
pub use resolve::{ImportResolver, Provenance, Resolution};
#[cfg(all(windows, feature = "diagnostics"))]
pub use stacks::{dump_stacks, dump_stacks_on_schedule};
#[cfg(windows)]
pub use threads::{disable_thread_calls, set_before_thread_shutdown};
#[cfg(any(windows, target_os = "macos"))]
pub use thunks::{UnimplementedPolicy, unimplemented_call_count};
#[cfg(windows)]
pub use windows::{LoadedImage, load};

/// `DLL_PROCESS_ATTACH`, for TLS callbacks.
pub const DLL_PROCESS_ATTACH: u32 = 1;
/// `DLL_PROCESS_DETACH`.
pub const DLL_PROCESS_DETACH: u32 = 0;
