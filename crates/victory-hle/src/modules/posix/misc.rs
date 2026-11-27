//! Errors, ids, time and facts about the machine.
#![allow(non_snake_case)]

use std::ffi::c_void;

use super::{Bool, FALSE, TRUE, current_thread_id, last_error, set_last_error, teb_field};
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(GetLastError),
        hle_export!(SetLastError),
        hle_export!(GetCurrentProcessId),
        hle_export!(GetCurrentThreadId),
        hle_export!(GetCurrentProcess),
        hle_export!(GetCurrentThread),
        hle_export!(GetCurrentThreadStackLimits),
        hle_export!(IsDebuggerPresent),
        hle_export!(IsProcessorFeaturePresent),
        hle_export!(EncodePointer),
        hle_export!(DecodePointer),
        hle_export!(QueryPerformanceCounter),
        hle_export!(QueryPerformanceFrequency),
        hle_export!(GetTickCount64),
        hle_export!(GetSystemTimeAsFileTime),
        hle_export!(GetSystemTimePreciseAsFileTime),
        hle_export!(Sleep),
        hle_export!(SwitchToThread),
    ]
}

extern "win64" fn GetLastError() -> u32 {
    last_error()
}

extern "win64" fn SetLastError(code: u32) {
    set_last_error(code)
}

extern "win64" fn GetCurrentProcessId() -> u32 {
    std::process::id()
}

extern "win64" fn GetCurrentThreadId() -> u32 {
    current_thread_id()
}

/// The pseudo handles Windows gives for "this process" and "this thread".
pub(crate) const CURRENT_PROCESS: usize = usize::MAX;
pub(crate) const CURRENT_THREAD: usize = usize::MAX - 1;

extern "win64" fn GetCurrentProcess() -> usize {
    CURRENT_PROCESS
}

extern "win64" fn GetCurrentThread() -> usize {
    CURRENT_THREAD
}

extern "win64" fn GetCurrentThreadStackLimits(low: *mut usize, high: *mut usize) {
    // SAFETY: the title's out pointers.
    unsafe {
        low.write(teb_field(0x10));
        high.write(teb_field(0x08));
    }
}

extern "win64" fn IsDebuggerPresent() -> Bool {
    FALSE
}

/// The x86-64 features a Zen 2 console has, which Rosetta runs.
extern "win64" fn IsProcessorFeaturePresent(feature: u32) -> Bool {
    const PRESENT: [u32; 12] = [
        3,  // PF_MMX_INSTRUCTIONS_AVAILABLE
        6,  // PF_XMMI_INSTRUCTIONS_AVAILABLE
        8,  // PF_RDTSC_INSTRUCTION_AVAILABLE
        10, // PF_XMMI64_INSTRUCTIONS_AVAILABLE
        12, // PF_NX_ENABLED
        13, // PF_SSE3_INSTRUCTIONS_AVAILABLE
        14, // PF_COMPARE_EXCHANGE128
        36, // PF_SSSE3_INSTRUCTIONS_AVAILABLE
        37, // PF_SSE4_1_INSTRUCTIONS_AVAILABLE
        38, // PF_SSE4_2_INSTRUCTIONS_AVAILABLE
        39, // PF_AVX_INSTRUCTIONS_AVAILABLE
        40, // PF_AVX2_INSTRUCTIONS_AVAILABLE
    ];
    PRESENT.contains(&feature) as Bool
}

/// ponytail: pointers are not obscured; they only need to round-trip.
extern "win64" fn EncodePointer(p: *mut c_void) -> *mut c_void {
    p
}

extern "win64" fn DecodePointer(p: *mut c_void) -> *mut c_void {
    p
}

/// Counter ticks per second: 10 MHz, as Windows reports on most PCs.
const QPC_HZ: u64 = 10_000_000;

/// Nanoseconds of the monotonic clock (counting while the Mac sleeps, as
/// Windows' counter does).
fn monotonic_ns() -> u64 {
    unsafe extern "C" {
        fn clock_gettime_nsec_np(clock: libc::clockid_t) -> u64;
    }
    // SAFETY: a plain query.
    unsafe { clock_gettime_nsec_np(libc::CLOCK_MONOTONIC_RAW) }
}

extern "win64" fn QueryPerformanceCounter(count: *mut i64) -> Bool {
    // SAFETY: the title's out pointer.
    unsafe { count.write((monotonic_ns() / (1_000_000_000 / QPC_HZ)) as i64) };
    TRUE
}

extern "win64" fn QueryPerformanceFrequency(hz: *mut i64) -> Bool {
    // SAFETY: the title's out pointer.
    unsafe { hz.write(QPC_HZ as i64) };
    TRUE
}

extern "win64" fn GetTickCount64() -> u64 {
    monotonic_ns() / 1_000_000
}

/// 100 ns intervals between 1601-01-01 and 1970-01-01.
const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;

/// Now as a FILETIME: 100 ns intervals since 1601 (UTC).
pub(crate) fn filetime_now() -> u64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    UNIX_EPOCH_AS_FILETIME + (since.as_nanos() / 100) as u64
}

extern "win64" fn GetSystemTimeAsFileTime(out: *mut u64) {
    // SAFETY: the title's out pointer (FILETIME: two u32, 4-byte aligned).
    unsafe { out.write_unaligned(filetime_now()) };
}

extern "win64" fn GetSystemTimePreciseAsFileTime(out: *mut u64) {
    GetSystemTimeAsFileTime(out)
}

extern "win64" fn Sleep(ms: u32) {
    if ms == 0 {
        std::thread::yield_now();
    } else {
        std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
    }
}

extern "win64" fn SwitchToThread() -> Bool {
    std::thread::yield_now();
    TRUE
}
