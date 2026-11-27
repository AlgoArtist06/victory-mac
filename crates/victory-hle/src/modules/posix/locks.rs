//! Locks that live in the title's memory: critical sections, slim
//! reader/writer locks, condition variables, one-time initialisation,
//! interlocked lists, and fiber-local storage.
//!
//! A critical section and a slim lock hold an `os_unfair_lock` (four bytes,
//! zero when free, as `SRWLOCK_INIT` is). A condition variable is a counter
//! that wakers raise; waiters sleep on one of a few shared condvars.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use super::{Bool, FALSE, TRUE, current_thread_id, deadline, error, set_last_error};
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(InitializeCriticalSection),
        hle_export!(InitializeCriticalSectionAndSpinCount),
        hle_export!(InitializeCriticalSectionEx),
        hle_export!(DeleteCriticalSection),
        hle_export!(EnterCriticalSection),
        hle_export!(TryEnterCriticalSection),
        hle_export!(LeaveCriticalSection),
        hle_export!(InitializeSRWLock),
        hle_export!(AcquireSRWLockExclusive),
        hle_export!(TryAcquireSRWLockExclusive),
        hle_export!(ReleaseSRWLockExclusive),
        hle_export!(InitializeConditionVariable),
        hle_export!(WakeConditionVariable),
        hle_export!(WakeAllConditionVariable),
        hle_export!(SleepConditionVariableCS),
        hle_export!(SleepConditionVariableSRW),
        hle_export!(InitOnceExecuteOnce),
        hle_export!(InitializeSListHead),
        hle_export!(InterlockedPushEntrySList),
        hle_export!(InterlockedFlushSList),
        hle_export!(FlsAlloc),
        hle_export!(FlsFree),
        hle_export!(FlsGetValue),
        hle_export!(FlsSetValue),
    ]
}

/// `RTL_CRITICAL_SECTION`: debug info, lock count, recursion count, owner,
/// "semaphore" (our lock), spin count.
#[repr(C)]
struct CriticalSection {
    debug_info: usize,
    lock_count: i32,
    recursion: i32,
    owner: AtomicUsize,
    lock: AtomicU32,
    _pad: u32,
    spin_count: usize,
}

fn unfair(lock: &AtomicU32) -> *mut libc::os_unfair_lock {
    lock.as_ptr().cast()
}

extern "win64" fn InitializeCriticalSection(cs: *mut CriticalSection) {
    // SAFETY: the title's critical section, 40 bytes.
    unsafe {
        cs.write(CriticalSection {
            debug_info: 0,
            lock_count: -1,
            recursion: 0,
            owner: AtomicUsize::new(0),
            lock: AtomicU32::new(0),
            _pad: 0,
            spin_count: 0,
        })
    };
}

extern "win64" fn InitializeCriticalSectionAndSpinCount(cs: *mut CriticalSection, _: u32) -> Bool {
    InitializeCriticalSection(cs);
    TRUE
}

extern "win64" fn InitializeCriticalSectionEx(cs: *mut CriticalSection, _: u32, _: u32) -> Bool {
    InitializeCriticalSection(cs);
    TRUE
}

extern "win64" fn DeleteCriticalSection(_: *mut CriticalSection) {}

extern "win64" fn EnterCriticalSection(cs: *mut CriticalSection) {
    // SAFETY: an initialised critical section of the title.
    let cs = unsafe { &mut *cs };
    let me = current_thread_id() as usize;
    if cs.owner.load(Relaxed) != me {
        // SAFETY: our lock word inside it.
        unsafe { libc::os_unfair_lock_lock(unfair(&cs.lock)) };
        cs.owner.store(me, Relaxed);
    }
    cs.recursion += 1;
    cs.lock_count += 1;
}

extern "win64" fn TryEnterCriticalSection(cs: *mut CriticalSection) -> Bool {
    // SAFETY: as above.
    let cs = unsafe { &mut *cs };
    let me = current_thread_id() as usize;
    if cs.owner.load(Relaxed) != me {
        // SAFETY: our lock word inside it.
        if !unsafe { libc::os_unfair_lock_trylock(unfair(&cs.lock)) } {
            return FALSE;
        }
        cs.owner.store(me, Relaxed);
    }
    cs.recursion += 1;
    cs.lock_count += 1;
    TRUE
}

extern "win64" fn LeaveCriticalSection(cs: *mut CriticalSection) {
    // SAFETY: as above; the caller owns it.
    let cs = unsafe { &mut *cs };
    cs.lock_count -= 1;
    cs.recursion -= 1;
    if cs.recursion == 0 {
        cs.owner.store(0, Relaxed);
        // SAFETY: held by this thread.
        unsafe { libc::os_unfair_lock_unlock(unfair(&cs.lock)) };
    }
}

extern "win64" fn InitializeSRWLock(lock: *mut u64) {
    // SAFETY: the title's lock, 8 bytes.
    unsafe { lock.write(0) };
}

fn srw(lock: *mut u64) -> *mut libc::os_unfair_lock {
    lock.cast()
}

extern "win64" fn AcquireSRWLockExclusive(lock: *mut u64) {
    // SAFETY: the title's lock word.
    unsafe { libc::os_unfair_lock_lock(srw(lock)) };
}

extern "win64" fn TryAcquireSRWLockExclusive(lock: *mut u64) -> u8 {
    // SAFETY: as above.
    unsafe { libc::os_unfair_lock_trylock(srw(lock)) as u8 }
}

extern "win64" fn ReleaseSRWLockExclusive(lock: *mut u64) {
    // SAFETY: held by this thread.
    unsafe { libc::os_unfair_lock_unlock(srw(lock)) };
}

/// Waiters on condition variables sleep on one of these, by address.
/// ponytail: shared condvars wake unrelated waiters spuriously (allowed);
/// per-variable queues if that shows in profiles.
static SHARDS: [(Mutex<()>, Condvar); 32] = [const { (Mutex::new(()), Condvar::new()) }; 32];

fn shard(cv: *mut u64) -> &'static (Mutex<()>, Condvar) {
    &SHARDS[(cv as usize >> 3) % SHARDS.len()]
}

fn counter<'a>(cv: *mut u64) -> &'a AtomicU64 {
    // SAFETY: the title's condition variable, 8 bytes, aligned.
    unsafe { AtomicU64::from_ptr(cv) }
}

extern "win64" fn InitializeConditionVariable(cv: *mut u64) {
    counter(cv).store(0, Relaxed);
}

extern "win64" fn WakeConditionVariable(cv: *mut u64) {
    WakeAllConditionVariable(cv)
}

extern "win64" fn WakeAllConditionVariable(cv: *mut u64) {
    counter(cv).fetch_add(1, SeqCst);
    let (lock, wake) = shard(cv);
    drop(lock.lock().unwrap_or_else(|e| e.into_inner()));
    wake.notify_all();
}

/// Waits on `cv` with the caller's lock let go by `release` and taken
/// again by `take`. FALSE with `ERROR_TIMEOUT` if `ms` ran out.
fn sleep_on(cv: *mut u64, ms: u32, release: impl FnOnce(), take: impl FnOnce()) -> Bool {
    let until: Option<Instant> = deadline(ms);
    let (lock, wake) = shard(cv);
    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    let seen = counter(cv).load(SeqCst);
    release();
    let mut woken = true;
    while counter(cv).load(SeqCst) == seen {
        match until {
            None => guard = wake.wait(guard).unwrap_or_else(|e| e.into_inner()),
            Some(until) => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    woken = false;
                    break;
                }
                guard = wake
                    .wait_timeout(guard, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        }
    }
    drop(guard);
    take();
    if woken {
        TRUE
    } else {
        set_last_error(error::TIMEOUT);
        FALSE
    }
}

extern "win64" fn SleepConditionVariableCS(cv: *mut u64, cs: *mut CriticalSection, ms: u32) -> Bool {
    // A recursively held section is let go fully and taken back as deep.
    // SAFETY: the caller holds `cs`.
    let depth = unsafe { (*cs).recursion };
    sleep_on(
        cv,
        ms,
        || (0..depth).for_each(|_| LeaveCriticalSection(cs)),
        || (0..depth).for_each(|_| EnterCriticalSection(cs)),
    )
}

extern "win64" fn SleepConditionVariableSRW(cv: *mut u64, lock: *mut u64, ms: u32, _flags: u32) -> Bool {
    sleep_on(
        cv,
        ms,
        || ReleaseSRWLockExclusive(lock),
        || AcquireSRWLockExclusive(lock),
    )
}

type InitFn = unsafe extern "win64" fn(*mut u64, *mut c_void, *mut *mut c_void) -> Bool;

/// `INIT_ONCE`: 0 not run, 1 running, 2 done.
extern "win64" fn InitOnceExecuteOnce(
    once: *mut u64,
    init: InitFn,
    param: *mut c_void,
    context: *mut *mut c_void,
) -> Bool {
    let state = counter(once);
    loop {
        match state.compare_exchange(0, 1, Acquire, Acquire) {
            Ok(_) => {
                // SAFETY: the title's callback with its own arguments.
                let ok = unsafe { init(once, param, context) };
                state.store(if ok != 0 { 2 } else { 0 }, Release);
                return ok;
            }
            Err(2) => return TRUE,
            Err(_) => std::thread::yield_now(),
        }
    }
}

/// `SLIST_HEADER` on x64: depth and sequence in the first word, the first
/// entry's address in the second. ponytail: one lock for every list.
static SLIST: Mutex<()> = Mutex::new(());

extern "win64" fn InitializeSListHead(head: *mut [u64; 2]) {
    // SAFETY: the title's 16-byte header.
    unsafe { head.write([0, 0]) };
}

extern "win64" fn InterlockedPushEntrySList(head: *mut [u64; 2], entry: *mut u64) -> *mut u64 {
    let _held = SLIST.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the title's header and entry.
    unsafe {
        let [count, first] = head.read();
        entry.write(first);
        let depth = (count & 0xffff) + 1;
        let sequence = (count >> 16) + 1;
        head.write([depth & 0xffff | sequence << 16, entry as u64]);
        first as *mut u64
    }
}

extern "win64" fn InterlockedFlushSList(head: *mut [u64; 2]) -> *mut u64 {
    let _held = SLIST.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the title's header.
    unsafe {
        let [count, first] = head.read();
        head.write([count & !0xffff, 0]);
        first as *mut u64
    }
}

/// Fiber-local storage: with no fibers, per thread.
/// ponytail: the callbacks given to `FlsAlloc` are not called at thread end.
static FLS_USED: Mutex<Vec<bool>> = Mutex::new(Vec::new());

thread_local! {
    static FLS: std::cell::RefCell<Vec<usize>> = const { std::cell::RefCell::new(Vec::new()) };
}

extern "win64" fn FlsAlloc(_callback: *const c_void) -> u32 {
    let mut used = FLS_USED.lock().unwrap_or_else(|e| e.into_inner());
    let index = used.iter().position(|u| !u).unwrap_or(used.len());
    if index == used.len() {
        used.push(true);
    } else {
        used[index] = true;
    }
    index as u32
}

extern "win64" fn FlsFree(index: u32) -> Bool {
    let mut used = FLS_USED.lock().unwrap_or_else(|e| e.into_inner());
    match used.get_mut(index as usize) {
        Some(slot) => {
            *slot = false;
            TRUE
        }
        None => {
            set_last_error(error::INVALID_PARAMETER);
            FALSE
        }
    }
}

extern "win64" fn FlsGetValue(index: u32) -> usize {
    set_last_error(0);
    FLS.with_borrow(|v| v.get(index as usize).copied().unwrap_or(0))
}

extern "win64" fn FlsSetValue(index: u32, value: usize) -> Bool {
    FLS.with_borrow_mut(|v| {
        if v.len() <= index as usize {
            v.resize(index as usize + 1, 0);
        }
        v[index as usize] = value;
    });
    TRUE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nested_critical_section_sleeps_and_wakes() {
        let cs = Box::leak(Box::new(std::mem::MaybeUninit::<CriticalSection>::uninit())).as_mut_ptr();
        let cv: &'static mut u64 = Box::leak(Box::new(0));
        let cv = cv as *mut u64 as usize;
        InitializeCriticalSection(cs);
        let cs = cs as usize;
        EnterCriticalSection(cs as *mut _);
        EnterCriticalSection(cs as *mut _);
        let timed_out = SleepConditionVariableCS(cv as *mut u64, cs as *mut _, 5);
        assert_eq!(timed_out, FALSE, "no waker: times out, with the section held again");
        let waker = std::thread::spawn(move || {
            EnterCriticalSection(cs as *mut _);
            WakeAllConditionVariable(cv as *mut u64);
            LeaveCriticalSection(cs as *mut _);
        });
        assert_eq!(SleepConditionVariableCS(cv as *mut u64, cs as *mut _, 5000), TRUE, "woken");
        // SAFETY: the section we initialised.
        assert_eq!(unsafe { (*(cs as *mut CriticalSection)).recursion }, 2, "held as deep as before");
        LeaveCriticalSection(cs as *mut _);
        LeaveCriticalSection(cs as *mut _);
        waker.join().unwrap();
    }
}
