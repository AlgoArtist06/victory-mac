//! Kernel objects behind handles: events, mutexes, semaphores and threads,
//! and the waits on them.
//!
//! ponytail: one lock and one condvar for every object, so a wait on several
//! kinds at once is simple; per-object waiters if contention shows.
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Condvar, LazyLock, Mutex, MutexGuard};
use std::time::Instant;

use super::misc::{CURRENT_PROCESS, CURRENT_THREAD};
use super::{Bool, FALSE, TRUE, current_thread_id, deadline, error, set_last_error};
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(CloseHandle),
        hle_export!(CreateEventA),
        hle_export!(CreateEventW),
        hle_export!(SetEvent),
        hle_export!(ResetEvent),
        hle_export!(CreateMutexA),
        hle_export!(ReleaseMutex),
        hle_export!(CreateSemaphoreW),
        hle_export!(CreateSemaphoreExW),
        hle_export!(ReleaseSemaphore),
        hle_export!(WaitForSingleObject),
        hle_export!(WaitForSingleObjectEx),
        hle_export!(WaitForMultipleObjects),
        hle_export!(CreateThread),
        hle_export!(ResumeThread),
        hle_export!(GetExitCodeThread),
        hle_export!(GetThreadId),
    ]
}

enum Object {
    Event { manual: bool, set: bool },
    Mutex { owner: u32, depth: u32 },
    Semaphore { count: i32, max: i32 },
    Thread { id: u32, exit: Option<u32>, suspended: bool },
}

impl Object {
    fn signalled(&self, me: u32) -> bool {
        match *self {
            Object::Event { set, .. } => set,
            Object::Mutex { owner, .. } => owner == 0 || owner == me,
            Object::Semaphore { count, .. } => count > 0,
            Object::Thread { exit, .. } => exit.is_some(),
        }
    }

    /// What a satisfied wait does to the object.
    fn take(&mut self, me: u32) {
        match self {
            Object::Event { manual: false, set } => *set = false,
            Object::Mutex { owner, depth } => {
                *owner = me;
                *depth += 1;
            }
            Object::Semaphore { count, .. } => *count -= 1,
            _ => {}
        }
    }
}

struct Table {
    next: usize,
    objects: HashMap<usize, Object>,
}

/// Handles start here, apart from the module bases and other handles the
/// runtime gives out.
const FIRST_HANDLE: usize = 0x7e00_0000;

static TABLE: LazyLock<Mutex<Table>> = LazyLock::new(|| {
    Mutex::new(Table {
        next: FIRST_HANDLE,
        objects: HashMap::new(),
    })
});
static CHANGED: Condvar = Condvar::new();

fn table() -> MutexGuard<'static, Table> {
    TABLE.lock().unwrap_or_else(|e| e.into_inner())
}

fn insert(object: Object) -> usize {
    let mut t = table();
    let handle = t.next;
    t.next += 4;
    t.objects.insert(handle, object);
    handle
}

/// Runs `f` on the object behind `handle` and wakes the waiters; FALSE with
/// `ERROR_INVALID_HANDLE` if there is none.
fn change(handle: usize, f: impl FnOnce(&mut Object) -> Result<(), u32>) -> Bool {
    let mut t = table();
    let result = match t.objects.get_mut(&handle) {
        Some(object) => f(object),
        None => Err(error::INVALID_HANDLE),
    };
    drop(t);
    CHANGED.notify_all();
    match result {
        Ok(()) => TRUE,
        Err(code) => {
            set_last_error(code);
            FALSE
        }
    }
}

extern "win64" fn CloseHandle(handle: usize) -> Bool {
    if handle == CURRENT_PROCESS || handle == CURRENT_THREAD {
        return TRUE;
    }
    if table().objects.remove(&handle).is_some() {
        TRUE
    } else {
        set_last_error(error::INVALID_HANDLE);
        FALSE
    }
}

/// ponytail: named objects are not shared by name; each call makes one.
extern "win64" fn CreateEventW(_: *const c_void, manual: Bool, initial: Bool, _name: *const u16) -> usize {
    insert(Object::Event {
        manual: manual != 0,
        set: initial != 0,
    })
}

extern "win64" fn CreateEventA(attributes: *const c_void, manual: Bool, initial: Bool, _: *const u8) -> usize {
    CreateEventW(attributes, manual, initial, std::ptr::null())
}

extern "win64" fn SetEvent(handle: usize) -> Bool {
    change(handle, |o| match o {
        Object::Event { set, .. } => {
            *set = true;
            Ok(())
        }
        _ => Err(error::INVALID_HANDLE),
    })
}

extern "win64" fn ResetEvent(handle: usize) -> Bool {
    change(handle, |o| match o {
        Object::Event { set, .. } => {
            *set = false;
            Ok(())
        }
        _ => Err(error::INVALID_HANDLE),
    })
}

extern "win64" fn CreateMutexA(_: *const c_void, owned: Bool, _: *const u8) -> usize {
    let me = current_thread_id();
    insert(Object::Mutex {
        owner: if owned != 0 { me } else { 0 },
        depth: u32::from(owned != 0),
    })
}

extern "win64" fn ReleaseMutex(handle: usize) -> Bool {
    let me = current_thread_id();
    change(handle, |o| match o {
        Object::Mutex { owner, depth } if *owner == me => {
            *depth -= 1;
            if *depth == 0 {
                *owner = 0;
            }
            Ok(())
        }
        Object::Mutex { .. } => Err(error::NOT_OWNER),
        _ => Err(error::INVALID_HANDLE),
    })
}

extern "win64" fn CreateSemaphoreW(_: *const c_void, initial: i32, max: i32, _: *const u16) -> usize {
    insert(Object::Semaphore {
        count: initial,
        max,
    })
}

extern "win64" fn CreateSemaphoreExW(
    attributes: *const c_void,
    initial: i32,
    max: i32,
    name: *const u16,
    _: u32,
    _: u32,
) -> usize {
    CreateSemaphoreW(attributes, initial, max, name)
}

extern "win64" fn ReleaseSemaphore(handle: usize, add: i32, previous: *mut i32) -> Bool {
    change(handle, |o| match o {
        Object::Semaphore { count, max } if *count + add <= *max => {
            if !previous.is_null() {
                // SAFETY: the title's out pointer.
                unsafe { previous.write(*count) };
            }
            *count += add;
            Ok(())
        }
        Object::Semaphore { .. } => Err(298), // ERROR_TOO_MANY_POSTS
        _ => Err(error::INVALID_HANDLE),
    })
}

const WAIT_OBJECT_0: u32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const WAIT_FAILED: u32 = u32::MAX;

/// Waits until one (`all`: every one) of `handles` is signalled.
fn wait(handles: &[usize], all: bool, ms: u32) -> u32 {
    let me = current_thread_id();
    let until = deadline(ms);
    let mut t = table();
    loop {
        if let Some(missing) = handles.iter().find(|h| !t.objects.contains_key(h)) {
            tracing::warn!(handle = format_args!("{missing:#x}"), "wait on an unknown handle");
            set_last_error(error::INVALID_HANDLE);
            return WAIT_FAILED;
        }
        let ready = |h: &usize| t.objects[h].signalled(me);
        if all {
            if handles.iter().all(ready) {
                for h in handles {
                    t.objects.get_mut(h).expect("checked above").take(me);
                }
                return WAIT_OBJECT_0;
            }
        } else if let Some(i) = handles.iter().position(ready) {
            t.objects.get_mut(&handles[i]).expect("checked above").take(me);
            return WAIT_OBJECT_0 + i as u32;
        }
        t = match until {
            None => CHANGED.wait(t).unwrap_or_else(|e| e.into_inner()),
            Some(until) => {
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return WAIT_TIMEOUT;
                }
                CHANGED
                    .wait_timeout(t, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0
            }
        };
    }
}

extern "win64" fn WaitForSingleObject(handle: usize, ms: u32) -> u32 {
    wait(&[handle], false, ms)
}

extern "win64" fn WaitForSingleObjectEx(handle: usize, ms: u32, _alertable: Bool) -> u32 {
    wait(&[handle], false, ms)
}

extern "win64" fn WaitForMultipleObjects(count: u32, handles: *const usize, all: Bool, ms: u32) -> u32 {
    // SAFETY: the title's array of `count` handles.
    let handles = unsafe { std::slice::from_raw_parts(handles, count as usize) };
    wait(handles, all != 0, ms)
}

type ThreadStart = unsafe extern "win64" fn(*mut c_void) -> u32;

const CREATE_SUSPENDED: u32 = 4;

extern "win64" fn CreateThread(
    _: *const c_void,
    stack: usize,
    start: ThreadStart,
    param: *mut c_void,
    flags: u32,
    id_out: *mut u32,
) -> usize {
    let suspended = flags & CREATE_SUSPENDED != 0;
    let handle = insert(Object::Thread {
        id: 0,
        exit: None,
        suspended,
    });
    let param = param as usize;
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .stack_size(stack.max(1 << 20))
        .spawn(move || {
            // SAFETY: a title thread runs title code from here on.
            unsafe { victory_loader::enter_title_thread() };
            let id = current_thread_id();
            let _ = tx.send(id);
            let mut t = table();
            if let Some(Object::Thread { id: slot, .. }) = t.objects.get_mut(&handle) {
                *slot = id;
            }
            while matches!(t.objects.get(&handle), Some(Object::Thread { suspended: true, .. })) {
                t = CHANGED.wait(t).unwrap_or_else(|e| e.into_inner());
            }
            drop(t);
            // SAFETY: the title's thread routine with its own argument.
            let code = unsafe { start(param as *mut c_void) };
            change(handle, |o| {
                if let Object::Thread { exit, .. } = o {
                    *exit = Some(code);
                }
                Ok(())
            });
        });
    if spawned.is_err() {
        table().objects.remove(&handle);
        set_last_error(error::NOT_ENOUGH_MEMORY);
        return 0;
    }
    let id = rx.recv().unwrap_or(0);
    if !id_out.is_null() {
        // SAFETY: the title's out pointer.
        unsafe { id_out.write(id) };
    }
    handle
}

extern "win64" fn ResumeThread(handle: usize) -> u32 {
    let mut was = 0;
    change(handle, |o| match o {
        Object::Thread { suspended, .. } => {
            was = u32::from(*suspended);
            *suspended = false;
            Ok(())
        }
        _ => Err(error::INVALID_HANDLE),
    });
    was
}

const STILL_ACTIVE: u32 = 259;

extern "win64" fn GetExitCodeThread(handle: usize, code: *mut u32) -> Bool {
    let exit = match table().objects.get(&handle) {
        Some(Object::Thread { exit, .. }) => exit.unwrap_or(STILL_ACTIVE),
        _ => {
            set_last_error(error::INVALID_HANDLE);
            return FALSE;
        }
    };
    // SAFETY: the title's out pointer.
    unsafe { code.write(exit) };
    TRUE
}

extern "win64" fn GetThreadId(handle: usize) -> u32 {
    if handle == CURRENT_THREAD {
        return current_thread_id();
    }
    match table().objects.get(&handle) {
        Some(Object::Thread { id, .. }) => *id,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "win64" fn answer(param: *mut c_void) -> u32 {
        SetEvent(param as usize);
        42
    }

    #[test]
    fn events_threads_and_waits_behave_as_on_windows() {
        let event = CreateEventW(std::ptr::null(), FALSE, FALSE, std::ptr::null());
        assert_eq!(WaitForSingleObject(event, 10), WAIT_TIMEOUT, "unset event times out");
        let thread = CreateThread(std::ptr::null(), 0, answer, event as *mut c_void, 0, std::ptr::null_mut());
        assert_eq!(WaitForSingleObject(event, INFINITE_MS), WAIT_OBJECT_0, "the thread set it");
        assert_eq!(WaitForSingleObject(event, 10), WAIT_TIMEOUT, "auto-reset: one wait takes it");
        assert_eq!(WaitForSingleObject(thread, INFINITE_MS), WAIT_OBJECT_0, "thread ends");
        let mut code = 0;
        assert_eq!(GetExitCodeThread(thread, &mut code), TRUE);
        assert_eq!(code, 42);
        let manual = CreateEventW(std::ptr::null(), TRUE, TRUE, std::ptr::null());
        let both = [manual, thread];
        assert_eq!(WaitForMultipleObjects(2, both.as_ptr(), TRUE, 0), WAIT_OBJECT_0);
        assert_eq!(WaitForSingleObject(manual, 0), WAIT_OBJECT_0, "manual-reset stays set");
        for h in [event, thread, manual] {
            assert_eq!(CloseHandle(h), TRUE);
        }
        assert_eq!(WaitForSingleObject(event, 0), WAIT_FAILED, "closed handle");
    }

    const INFINITE_MS: u32 = super::super::INFINITE;
}
