//! `xmem.dll` and the title's `VirtualAlloc` family on macOS.
//!
//! Reservations are Mach allocations kept `PROT_NONE` until committed, at
//! addresses inside the range the title's flags allow (the low 4 GiB for
//! 32-bit memory: the app is linked with a small `__PAGEZERO` for it).
//! Each 4 KiB page's state is ours to report to `VirtualQuery`.
//!
//! Physical pages are 64 KiB frames of Mach named memory entries; mapping
//! them (`XMemMapPhysicalPages`) puts the entry's frames in place with
//! `mach_vm_map`, so a frame can show at several addresses, as on the
//! console.
#![allow(non_snake_case)]

use std::alloc::{Layout, alloc, dealloc};
use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::modules::posix::set_last_error;
use crate::{HleModule, hle_export};

const SMALL: usize = 4 << 10;
const PAGE: usize = 64 << 10;
const MIB: usize = 1 << 20;

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_DECOMMIT: u32 = 0x4000;
const MEM_RELEASE: u32 = 0x8000;
const MEM_FREE: u32 = 0x1_0000;
const MEM_PRIVATE: u32 = 0x2_0000;
const MEM_MAPPED: u32 = 0x4_0000;
const MEM_REPLACE_PLACEHOLDER: u32 = 0x4000;
const MEM_COALESCE_PLACEHOLDERS: u32 = 0x1;
const MEM_PRESERVE_PLACEHOLDER: u32 = 0x2;
const PAGES_2M: u32 = 0x2000_0000;
const PAGES_64K_BIT: u32 = 0x40_0000;
const PAGES_64K: u32 = PAGES_2M | PAGES_64K_BIT;
const PAGE_NOACCESS: u32 = 1;
const PAGE_READWRITE: u32 = 4;
/// The host's protection bits; the console's GPU bits lie above.
const HOST_PROTECTION: u32 = 0xff;
const GRAPHICS: u64 = 3;
const ADDRESS_32BIT: u64 = 4;

#[allow(non_camel_case_types)]
type kern_return_t = i32;

unsafe extern "C" {
    static mach_task_self_: u32;
    fn mach_vm_allocate(task: u32, address: *mut u64, size: u64, flags: i32) -> kern_return_t;
    fn mach_vm_deallocate(task: u32, address: u64, size: u64) -> kern_return_t;
    fn mach_vm_map(task: u32, address: *mut u64, size: u64, mask: u64, flags: i32, object: u32, offset: u64, copy: i32, cur: i32, max: i32, inherit: u32) -> kern_return_t;
    fn mach_make_memory_entry_64(task: u32, size: *mut u64, offset: u64, permission: i32, handle: *mut u32, parent: u32) -> kern_return_t;
    fn mach_port_deallocate(task: u32, name: u32) -> kern_return_t;
    fn mach_vm_region(task: u32, address: *mut u64, size: *mut u64, flavor: i32, info: *mut i32, count: *mut u32, object: *mut u32) -> kern_return_t;
}

const VM_FLAGS_FIXED: i32 = 0;
const VM_FLAGS_OVERWRITE: i32 = 0x4000;
const VM_PROT_RW: i32 = 3;
const MAP_MEM_NAMED_CREATE: i32 = 0x2_0000;

fn task() -> u32 {
    // SAFETY: the process's own task port, set before main.
    unsafe { mach_task_self_ }
}

/// The host protection for a Windows one.
fn host_protection(protect: u32) -> i32 {
    match protect & HOST_PROTECTION {
        0x02 => libc::PROT_READ,
        0x04 | 0x08 => libc::PROT_READ | libc::PROT_WRITE,
        0x10 | 0x20 => libc::PROT_READ | libc::PROT_EXEC,
        0x40 | 0x80 => libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        _ => libc::PROT_NONE,
    }
}

/// Fresh zeroed memory over `[at, at + len)` with no access, replacing
/// whatever was mapped there.
fn clear(at: usize, len: usize) -> bool {
    let mut address = at as u64;
    // SAFETY: inside one of our reservations.
    unsafe {
        mach_vm_allocate(task(), &mut address, len as u64, VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE) == 0
            && libc::mprotect(at as *mut c_void, len, libc::PROT_NONE) == 0
    }
}

/// A reservation: its first protection and each 4 KiB page's state (0
/// reserved, else the committed page's Windows protection), and which
/// pages show physical memory.
struct Reservation {
    size: usize,
    protect: u32,
    pages: Vec<u32>,
    physical: Vec<bool>,
}

struct Space {
    reservations: BTreeMap<usize, Reservation>,
    /// Where the next search for room starts, below and above 4 GiB.
    cursor: [usize; 2],
}

static SPACE: Mutex<Space> = Mutex::new(Space {
    reservations: BTreeMap::new(),
    cursor: [0, 0],
});

fn space() -> MutexGuard<'static, Space> {
    SPACE.lock().unwrap_or_else(|e| e.into_inner())
}

impl Space {
    /// The reservation holding `at`.
    fn holding(&mut self, at: usize) -> Option<(usize, &mut Reservation)> {
        let (&base, r) = self.reservations.range_mut(..=at).next_back()?;
        (at < base + r.size).then_some((base, r))
    }

    /// Room for `size` bytes, aligned, inside `[lowest, highest]`, taken
    /// at once (no other mapping can land there before it is ours).
    fn find_room(&mut self, lowest: usize, highest: usize, size: usize, align: usize) -> Option<usize> {
        let which = usize::from(highest > u32::MAX as usize);
        let mut at = self.cursor[which].max(lowest).next_multiple_of(align);
        let mut wrapped = false;
        loop {
            if at.checked_add(size).is_none_or(|end| end > highest + 1) {
                if wrapped {
                    return None;
                }
                wrapped = true;
                at = lowest.next_multiple_of(align);
                continue;
            }
            let mut address = at as u64;
            // SAFETY: a fixed allocation that fails rather than replace.
            if unsafe { mach_vm_allocate(task(), &mut address, size as u64, VM_FLAGS_FIXED) } == 0 {
                self.cursor[which] = at + size;
                return Some(at);
            }
            // Past whatever is there.
            let (mut region, mut len) = (at as u64, 0u64);
            let mut info = [0i32; 16];
            let mut count = 9u32;
            let mut object = 0u32;
            // SAFETY: a query into our buffers (VM_REGION_BASIC_INFO_64).
            let found = unsafe { mach_vm_region(task(), &mut region, &mut len, 9, info.as_mut_ptr(), &mut count, &mut object) } == 0;
            at = if found && region <= at as u64 + size as u64 {
                ((region + len) as usize).next_multiple_of(align)
            } else {
                (at + align).next_multiple_of(align)
            };
        }
    }
}

/// The addresses XMem `flags` allow, first and last: the low 4 GiB for
/// 32-bit memory; else from 4 GiB up to 46 down to 40 bits from the field
/// at bit 12, or 40 bits without one (as on Windows: decision 0001).
fn address_range(flags: u64) -> (usize, usize) {
    let bits = match (flags & ADDRESS_32BIT, flags >> 12 & 7) {
        (0, 0) => 40,
        (0, field) => 47 - field,
        _ => return (0x1000_0000, u32::MAX as usize),
    };
    (1 << 32, (1 << bits) - 1)
}

/// Commits `[at, at + len)` inside reservation `r` (based at `base`).
fn commit(base: usize, r: &mut Reservation, at: usize, len: usize, protect: u32) -> bool {
    let first = (at - base) / SMALL;
    let last = (at + len - base).div_ceil(SMALL);
    let start = base + first * SMALL;
    let len = (last - first) * SMALL;
    // SAFETY: pages of our reservation.
    if unsafe { libc::mprotect(start as *mut c_void, len, host_protection(protect)) } != 0 {
        return false;
    }
    let protect = match protect & HOST_PROTECTION {
        0 => PAGE_READWRITE,
        p => p,
    };
    r.pages[first..last].fill(protect);
    true
}

fn allocate(base: usize, size: usize, kind: u32, flags: u64, protect: u32) -> usize {
    let mut s = space();
    if size == 0 {
        set_last_error(87);
        return 0;
    }
    let size_rounded = size.next_multiple_of(SMALL);
    // Inside an existing reservation: commit (or nothing to do).
    if base != 0
        && let Some((rbase, r)) = s.holding(base)
    {
        if kind & MEM_COMMIT != 0 && kind & MEM_REPLACE_PLACEHOLDER == 0 {
            if base + size > rbase + r.size || !commit(rbase, r, base, size, protect) {
                set_last_error(487); // ERROR_INVALID_ADDRESS
                return 0;
            }
        }
        return base & !(SMALL - 1);
    }
    // Without an address a request reserves too (as Windows' path does).
    let align = if kind & PAGES_64K == PAGES_2M { 2 * MIB } else { PAGE };
    let at = if base != 0 {
        let at = base & !(PAGE - 1);
        let mut address = at as u64;
        let len = (base + size - at).next_multiple_of(SMALL);
        // SAFETY: a fixed allocation that fails rather than replace.
        if unsafe { mach_vm_allocate(task(), &mut address, len as u64, VM_FLAGS_FIXED) } != 0 {
            set_last_error(487);
            return 0;
        }
        at
    } else {
        let (lowest, highest) = address_range(flags);
        match s.find_room(lowest, highest, size_rounded, align) {
            Some(at) => at,
            None => {
                tracing::warn!(size, flags = format_args!("{flags:#x}"), "XMemVirtualAlloc: no room");
                set_last_error(8);
                return 0;
            }
        }
    };
    let len = (base.max(at) + size - at).next_multiple_of(SMALL);
    // SAFETY: the allocation we just made.
    unsafe { libc::mprotect(at as *mut c_void, len, libc::PROT_NONE) };
    let mut r = Reservation {
        size: len,
        protect,
        pages: vec![0; len / SMALL],
        physical: vec![false; len / SMALL],
    };
    if kind & MEM_COMMIT != 0 {
        commit(at, &mut r, at, len, protect);
    }
    s.reservations.insert(at, r);
    at
}

pub extern "win64" fn XMemVirtualAlloc(base: *mut c_void, size: usize, kind: u32, flags: u64, protect: u32) -> *mut c_void {
    let at = allocate(base as usize, size, kind, flags, protect);
    tracing::debug!(
        size,
        kind = format_args!("{kind:#x}"),
        flags = format_args!("{flags:#x}"),
        protect = format_args!("{protect:#x}"),
        at = format_args!("{at:#x}"),
        "XMemVirtualAlloc"
    );
    at as *mut c_void
}

extern "win64" fn VirtualAlloc(base: *mut c_void, size: usize, kind: u32, protect: u32) -> *mut c_void {
    // Host memory has no console flags: anywhere from 4 GiB up.
    allocate(base as usize, size, kind, 0, protect) as *mut c_void
}

extern "win64" fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> i32 {
    let at = address as usize;
    let mut s = space();
    if kind & (MEM_PRESERVE_PLACEHOLDER | MEM_COALESCE_PLACEHOLDERS) != 0 {
        // Splitting and joining placeholders changes nothing here.
        return 1;
    }
    if kind & MEM_RELEASE != 0 {
        let Some(r) = s.reservations.remove(&at) else {
            set_last_error(487);
            return 0;
        };
        // SAFETY: our whole reservation.
        unsafe { mach_vm_deallocate(task(), at as u64, r.size as u64) };
        return 1;
    }
    if kind & MEM_DECOMMIT != 0 {
        let Some((base, r)) = s.holding(at) else {
            set_last_error(487);
            return 0;
        };
        let len = if size == 0 { r.size - (at - base) } else { size };
        let first = (at - base) / SMALL;
        let last = ((at + len - base).div_ceil(SMALL)).min(r.pages.len());
        clear(base + first * SMALL, (last - first) * SMALL);
        r.pages[first..last].fill(0);
        r.physical[first..last].fill(false);
        return 1;
    }
    set_last_error(87);
    0
}

/// `MEMORY_BASIC_INFORMATION`.
#[repr(C)]
#[derive(Default)]
struct BasicInformation {
    base: usize,
    allocation_base: usize,
    allocation_protect: u32,
    partition: u16,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

const _: () = assert!(size_of::<BasicInformation>() == 48);

fn query(address: usize) -> BasicInformation {
    let page = address & !(SMALL - 1);
    let mut s = space();
    if let Some((base, r)) = s.holding(page) {
        let i = (page - base) / SMALL;
        let (state, physical) = (r.pages[i], r.physical[i]);
        let run = r.pages[i..]
            .iter()
            .zip(&r.physical[i..])
            .take_while(|&(&p, &m)| p == state && m == physical)
            .count();
        return BasicInformation {
            base: page,
            allocation_base: base,
            allocation_protect: r.protect,
            partition: 0,
            region_size: run * SMALL,
            state: if state == 0 { MEM_RESERVE } else { MEM_COMMIT },
            protect: state,
            kind: if physical { MEM_MAPPED } else { MEM_PRIVATE },
        };
    }
    let next_ours = s.reservations.range(page..).next().map(|(&b, _)| b);
    drop(s);
    // Memory that is not ours: the host's description of it.
    let (mut region, mut len) = (page as u64, 0u64);
    let mut info = [0i32; 16];
    let mut count = 9u32;
    let mut object = 0u32;
    // SAFETY: a query into our buffers (VM_REGION_BASIC_INFO_64: protection first).
    let found = unsafe { mach_vm_region(task(), &mut region, &mut len, 9, info.as_mut_ptr(), &mut count, &mut object) } == 0;
    if found && region as usize <= page {
        let prot = info[0];
        let protect = match (prot & 1 != 0, prot & 2 != 0, prot & 4 != 0) {
            (_, true, true) => 0x40,
            (true, false, true) => 0x20,
            (_, true, false) => 0x04,
            (true, false, false) => 0x02,
            _ => PAGE_NOACCESS,
        };
        let base = victory_loader::image_containing(page).map_or(region as usize, |(b, _)| b);
        return BasicInformation {
            base: page,
            allocation_base: base,
            allocation_protect: protect,
            partition: 0,
            region_size: (region + len) as usize - page,
            state: MEM_COMMIT,
            protect,
            kind: MEM_PRIVATE,
        };
    }
    let end = [next_ours, found.then_some(region as usize)].into_iter().flatten().min().unwrap_or(page + PAGE);
    BasicInformation {
        base: page,
        region_size: end.saturating_sub(page).max(SMALL),
        state: MEM_FREE,
        protect: PAGE_NOACCESS,
        ..Default::default()
    }
}

/// Whether `len` bytes from `at` can be read now (for crash reports).
pub(crate) fn readable(at: usize, len: usize) -> bool {
    let info = query(at);
    info.state == MEM_COMMIT && host_protection(info.protect) & libc::PROT_READ != 0 && info.base + info.region_size >= at + len
}

extern "win64" fn VirtualQueryEx(_process: usize, address: *const c_void, out: *mut BasicInformation, len: usize) -> usize {
    if len < size_of::<BasicInformation>() {
        set_last_error(24); // ERROR_BAD_LENGTH
        return 0;
    }
    // SAFETY: the caller's struct.
    unsafe { out.write(query(address as usize)) };
    size_of::<BasicInformation>()
}

extern "win64" fn VirtualQuery(address: *const c_void, out: *mut BasicInformation, len: usize) -> usize {
    VirtualQueryEx(0, address, out, len)
}

/// XMem's query: the host's, then the memory's flags and tag.
#[repr(C)]
struct XMemInformation {
    host: BasicInformation,
    flags: u64,
    tag_code: u32,
    tag: [u8; 4],
}

pub extern "win64" fn XMemVirtualQuery(_process: usize, address: *const c_void, out: *mut XMemInformation) -> i32 {
    let host = query(address as usize);
    let flags = if host.state == MEM_COMMIT { GRAPHICS } else { 0 };
    // SAFETY: the caller's struct.
    unsafe {
        out.write(XMemInformation {
            host,
            flags,
            tag_code: 0,
            tag: [0; 4],
        })
    };
    1
}

/// A named Mach memory entry: physical pages, freed when no page and no
/// mapping holds it (the mappings keep the memory itself). The whole entry
/// is also mapped once at `window`, where we write into its pages
/// (DirectStorage destinations) wherever the title shows them.
struct Entry {
    port: u32,
    window: usize,
    len: usize,
}

impl Drop for Entry {
    fn drop(&mut self) {
        // SAFETY: our window on the entry and our send right on it.
        unsafe {
            mach_vm_deallocate(task(), self.window as u64, self.len as u64);
            mach_port_deallocate(task(), self.port);
        }
    }
}

/// Page numbers count 4 KiB frames, so a 64 KiB page's is a multiple of
/// 16: the title keeps them as `number >> 4` and rebuilds them `<< 4`.
const FRAMES: usize = PAGE / SMALL;

struct Physical {
    next: usize,
    pages: HashMap<usize, (Arc<Entry>, usize)>,
}

static PHYSICAL: Mutex<Option<Physical>> = Mutex::new(None);
static PHYSICAL_BYTES: AtomicUsize = AtomicUsize::new(0);

fn physical() -> MutexGuard<'static, Option<Physical>> {
    let mut p = PHYSICAL.lock().unwrap_or_else(|e| e.into_inner());
    p.get_or_insert_with(|| Physical {
        next: FRAMES,
        pages: HashMap::new(),
    });
    p
}

pub extern "win64" fn XMemAllocatePhysicalPages(flags: u64, count: *mut usize, pages: *mut usize) -> i32 {
    if count.is_null() || pages.is_null() {
        return 0;
    }
    // SAFETY: checked non-null.
    let n = unsafe { count.read() };
    let Some(size) = n.checked_mul(PAGE).filter(|&s| s > 0) else { return 0 };
    let mut size64 = size as u64;
    let mut port = 0u32;
    // SAFETY: a new named entry, zero-filled on first touch.
    let made = unsafe { mach_make_memory_entry_64(task(), &mut size64, 0, MAP_MEM_NAMED_CREATE | VM_PROT_RW, &mut port, 0) };
    if made != 0 || size64 < size as u64 {
        tracing::warn!(n, made, "XMemAllocatePhysicalPages failed");
        // SAFETY: checked non-null.
        unsafe { count.write(0) };
        return 0;
    }
    let mut window = 0u64;
    const VM_FLAGS_ANYWHERE: i32 = 1;
    // SAFETY: the whole entry, mapped somewhere of the kernel's choosing.
    let mapped = unsafe { mach_vm_map(task(), &mut window, size as u64, 0, VM_FLAGS_ANYWHERE, port, 0, 0, VM_PROT_RW, VM_PROT_RW, 0) };
    if mapped != 0 {
        tracing::warn!(n, mapped, "XMemAllocatePhysicalPages: no window on the pages");
        // SAFETY: our send right.
        unsafe { mach_port_deallocate(task(), port) };
        // SAFETY: checked non-null.
        unsafe { count.write(0) };
        return 0;
    }
    let entry = Arc::new(Entry { port, window: window as usize, len: size });
    let mut p = physical();
    let p = p.as_mut().expect("made above");
    let first = p.next;
    p.next += n * FRAMES;
    for index in 0..n {
        p.pages.insert(first + index * FRAMES, (entry.clone(), index));
        // SAFETY: the title passes room for `n` page numbers.
        unsafe { pages.add(index).write(first + index * FRAMES) };
    }
    PHYSICAL_BYTES.fetch_add(size, Ordering::Relaxed);
    tracing::debug!(n, flags = format_args!("{flags:#x}"), "XMemAllocatePhysicalPages");
    1
}

/// Pages still mapped stay readable where they are until unmapped.
pub extern "win64" fn XMemFreePhysicalPages(n: usize, pages: *const usize) -> i32 {
    if pages.is_null() {
        return 0;
    }
    // SAFETY: the title passes `n` page numbers.
    let numbers = unsafe { std::slice::from_raw_parts(pages, n) };
    let mut p = physical();
    let p = p.as_mut().expect("made in physical()");
    let mut freed = 0;
    for number in numbers {
        if p.pages.remove(number).is_some() {
            freed += PAGE;
        }
    }
    PHYSICAL_BYTES.fetch_sub(freed, Ordering::Relaxed);
    1
}

/// Shows physical `page` at `at`, or nothing (reserved) for `None`.
fn show(s: &mut Space, at: usize, page: Option<usize>) -> bool {
    let Some((base, r)) = s.holding(at) else {
        tracing::warn!(at = format_args!("{at:#x}"), "physical pages mapped outside any reservation");
        return false;
    };
    let first = (at - base) / SMALL;
    let last = (first + FRAMES).min(r.pages.len());
    match page {
        None => {
            if r.physical[first..last].iter().any(|&m| m) || r.pages[first..last].iter().any(|&p| p != 0) {
                clear(at, PAGE);
            }
            r.pages[first..last].fill(0);
            r.physical[first..last].fill(false);
            true
        }
        Some(number) => {
            let found = physical().as_ref().and_then(|p| p.pages.get(&number).cloned());
            let Some((entry, index)) = found else {
                tracing::warn!(page = number, "XMemMapPhysicalPages: unknown page");
                return false;
            };
            let mut address = at as u64;
            // SAFETY: a frame of our entry over a page of our reservation.
            let mapped = unsafe {
                mach_vm_map(task(), &mut address, PAGE as u64, 0, VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE, entry.port, (index * PAGE) as u64, 0, VM_PROT_RW, VM_PROT_RW, 0)
            };
            if mapped != 0 {
                tracing::warn!(mapped, at = format_args!("{at:#x}"), "mach_vm_map of a physical page failed");
                return false;
            }
            r.pages[first..last].fill(PAGE_READWRITE);
            r.physical[first..last].fill(true);
            true
        }
    }
}

pub extern "win64" fn XMemMapPhysicalPages(address: *mut c_void, n: usize, pages: *const usize) -> *mut c_void {
    let base = address as usize;
    // Read first: title memory is not touched under our lock.
    // SAFETY: the title passes `n` page numbers, or null.
    let pages = (!pages.is_null()).then(|| unsafe { std::slice::from_raw_parts(pages, n) }.to_vec());
    let mut s = space();
    for i in 0..n {
        let page = pages.as_ref().map(|p| p[i]).filter(|&p| p != 0);
        if !show(&mut s, base + i * PAGE, page) {
            return std::ptr::null_mut();
        }
    }
    address
}

/// One page at each address; null `pages`, or page number 0, unmaps.
pub extern "win64" fn XMemMapPhysicalPagesScatter(addresses: *const *mut c_void, n: usize, pages: *const usize) -> i32 {
    if addresses.is_null() {
        return 0;
    }
    // SAFETY: the title passes `n` addresses and, unless null, `n` pages.
    let wanted: Vec<(usize, usize)> = (0..n)
        .map(|i| unsafe { (*addresses.add(i) as usize, if pages.is_null() { 0 } else { *pages.add(i) }) })
        .collect();
    let mut s = space();
    for (at, page) in wanted {
        if !show(&mut s, at, (page != 0).then_some(page)) {
            return 0;
        }
    }
    1
}

/// The frame behind each 64 KiB of a destination that starts `offset`
/// bytes into `pages[0]`: `(entry, index)`, or `None` for an unknown page.
fn frames(pages: &[usize], offset: usize, len: usize) -> Vec<Option<(Arc<Entry>, usize)>> {
    let p = physical();
    let p = p.as_ref().expect("made in physical()");
    pages[..(offset + len).div_ceil(PAGE).min(pages.len())]
        .iter()
        .map(|n| p.pages.get(n).cloned())
        .collect()
}

/// Whether every page under `len` bytes from `offset` is one of ours.
pub(crate) fn pages_known(pages: &[usize], offset: usize, len: usize) -> bool {
    pages.len() * PAGE >= offset + len && frames(pages, offset, len).iter().all(Option::is_some)
}

/// Where the pages show (for DirectStorage's tracing): not tracked here.
pub(crate) fn pages_shown(_: &[usize], _: usize, _: usize) -> Vec<(usize, usize)> {
    Vec::new()
}

/// Writable bytes of consecutive frames of one entry.
pub(crate) struct PageWindow {
    at: usize,
    len: usize,
    _entry: Arc<Entry>,
}

impl PageWindow {
    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: inside the entry's window, which `_entry` keeps mapped.
        unsafe { std::slice::from_raw_parts_mut(self.at as *mut u8, self.len) }
    }
}

/// The destination's bytes as one slice, if its pages are consecutive
/// frames of one allocation (the usual case).
pub(crate) fn page_window(pages: &[usize], offset: usize, len: usize) -> Option<PageWindow> {
    let found = frames(pages, offset, len);
    let (first, index) = found.first()?.clone()?;
    let consecutive = found
        .iter()
        .enumerate()
        .all(|(k, f)| f.as_ref().is_some_and(|(e, i)| Arc::ptr_eq(e, &first) && *i == index + k));
    consecutive.then(|| PageWindow {
        at: first.window + index * PAGE + offset,
        len,
        _entry: first,
    })
}

/// Writes `data` into the pages from `offset` in the first; false if a
/// page is unknown.
pub(crate) fn write_physical(pages: &[usize], offset: usize, data: &[u8]) -> bool {
    let found = frames(pages, offset, data.len());
    if found.iter().any(Option::is_none) {
        return false;
    }
    let mut done = 0;
    while done < data.len() {
        let at = offset + done;
        let (entry, index) = found[at / PAGE].as_ref().expect("checked above");
        let within = at % PAGE;
        let n = (PAGE - within).min(data.len() - done);
        // SAFETY: inside the entry's window.
        unsafe { std::ptr::copy_nonoverlapping(data[done..].as_ptr(), (entry.window + index * PAGE + within) as *mut u8, n) };
        done += n;
    }
    true
}

/// The GPU reads title memory in place here: nothing to tell it.
pub(crate) fn pages_written(_: &[usize], _: usize, _: usize) {}

pub(crate) fn stream_notice() -> bool {
    false
}

pub(crate) fn map_costs() -> Option<String> {
    None
}

// `XMemAlloc` attributes: alignment as log2 (5 bits at 24), then the
// memory type, then bit 33: a 32-bit address.
const ALIGNMENT_SHIFT: u64 = 24;
const ALLOC_32BIT: u64 = 1 << 33;
/// Our block header: the block's size and alignment, below the pointer.
const HEADER: usize = 2 * size_of::<usize>();

fn layout(size: usize, attributes: u64) -> Option<Layout> {
    let align = 1usize << (attributes >> ALIGNMENT_SHIFT & 0x1f).max(4);
    Layout::from_size_align(size.checked_add(align)?, align).ok()
}

type AllocRoutine = extern "win64" fn(usize, u64) -> *mut c_void;
type FreeRoutine = extern "win64" fn(*mut c_void, u64);

/// The routines the title's exe exports as `__XMemAllocationHooks__`.
fn hooks() -> [usize; 2] {
    static AT: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let at = match AT.get() {
        Some(&at) => at,
        None => match crate::runtime::main_image() {
            Some(base) => *AT.get_or_init(|| {
                // SAFETY: the mapped title and a NUL-terminated name.
                let at = unsafe { victory_loader::export_of(base, c"__XMemAllocationHooks__".as_ptr().cast()) }.unwrap_or(0);
                tracing::info!(found = at != 0, "xmem allocation hooks");
                at
            }),
            None => 0,
        },
    };
    if at == 0 {
        return [0; 2];
    }
    // SAFETY: the export is two routine pointers.
    unsafe { (at as *const [usize; 2]).read() }
}

pub extern "win64" fn XMemAlloc(size: usize, attributes: u64) -> *mut c_void {
    tracing::trace!(size, attributes = format_args!("{attributes:#x}"), "XMemAlloc");
    match hooks()[0] {
        0 => XMemAllocDefault(size, attributes),
        hook => {
            // SAFETY: the title's own routine of this type.
            let alloc: AllocRoutine = unsafe { std::mem::transmute(hook) };
            alloc(size, attributes)
        }
    }
}

pub extern "win64" fn XMemFree(address: *mut c_void, attributes: u64) {
    match hooks()[1] {
        0 => XMemFreeDefault(address, attributes),
        hook => {
            // SAFETY: the title's own routine of this type.
            let free: FreeRoutine = unsafe { std::mem::transmute(hook) };
            free(address, attributes)
        }
    }
}

pub extern "win64" fn XMemAllocDefault(size: usize, attributes: u64) -> *mut c_void {
    if attributes & ALLOC_32BIT != 0 {
        return XMemVirtualAlloc(std::ptr::null_mut(), size, MEM_RESERVE | MEM_COMMIT, GRAPHICS | ADDRESS_32BIT, PAGE_READWRITE);
    }
    let Some(layout) = layout(size, attributes) else { return std::ptr::null_mut() };
    // SAFETY: the layout has a non-zero size.
    let block = unsafe { alloc(layout) };
    if block.is_null() {
        return block.cast();
    }
    // SAFETY: the block holds the alignment (at least the header), then
    // `size` bytes.
    unsafe {
        let user = block.add(layout.align());
        user.sub(HEADER).cast::<[usize; 2]>().write([layout.size(), layout.align()]);
        user.cast()
    }
}

pub extern "win64" fn XMemFreeDefault(address: *mut c_void, attributes: u64) {
    if address.is_null() {
        return;
    }
    if attributes & ALLOC_32BIT != 0 {
        VirtualFree(address, 0, MEM_RELEASE);
        return;
    }
    // SAFETY: a header-prefixed block from `XMemAllocDefault`.
    unsafe {
        let user = address.cast::<u8>();
        let [size, align] = user.sub(HEADER).cast::<[usize; 2]>().read();
        dealloc(user.sub(align), Layout::from_size_align_unchecked(size, align));
    }
}

pub extern "win64" fn XMemIsToolsMemAvailable() -> i32 {
    0
}

#[repr(C)]
#[derive(Default)]
pub struct WorkingSet {
    pub title_limit: usize,
    pub title_used: usize,
    pub tools_limit: usize,
    pub tools_used: usize,
    pub stack_used: u32,
    pub physical_pages: u32,
    pub gpu_optimal_limit: usize,
    pub gpu_optimal_used: usize,
    pub tools_gpu_optimal_limit: usize,
    pub tools_gpu_optimal_used: usize,
    pub kernel_pool_used: u32,
    pub kernel_pool_limit: u32,
}

static TRANSFERRED: AtomicUsize = AtomicUsize::new(0);

/// What the title has: its committed pages and physical pages.
fn title_used() -> usize {
    let committed: usize = space().reservations.values().map(|r| r.pages.iter().filter(|&&p| p != 0).count() * SMALL).sum();
    committed + PHYSICAL_BYTES.load(Ordering::Relaxed)
}

pub extern "win64" fn XMemGetWorkingSetStatistics(_which: u32, out: *mut WorkingSet) -> i32 {
    if out.is_null() {
        return 0;
    }
    let (gpu_optimal, standard) = victory_gpu::console::current().title_memory();
    let physical = PHYSICAL_BYTES.load(Ordering::Relaxed);
    let (limit, used) = (gpu_optimal + standard - TRANSFERRED.load(Ordering::Relaxed), title_used());
    tracing::debug!(which = _which, limit_mb = limit >> 20, used_mb = used >> 20, physical_mb = physical >> 20, "XMemGetWorkingSetStatistics");
    // SAFETY: the caller's struct.
    unsafe {
        out.write(WorkingSet {
            title_limit: limit,
            title_used: used,
            physical_pages: (physical / PAGE) as u32,
            gpu_optimal_limit: gpu_optimal,
            ..Default::default()
        })
    };
    1
}

pub extern "win64" fn XMemTransferMemory(kind: u32, size: usize) -> i32 {
    const TITLE_TO_SYSTEM: u32 = 0;
    tracing::info!(kind, size, "XMemTransferMemory");
    if size == 0 {
        return 1;
    }
    let Some(size) = size.checked_next_multiple_of(2 * MIB) else { return 0 };
    let standard = victory_gpu::console::current().title_memory().1;
    let moved = kind == TITLE_TO_SYSTEM
        && TRANSFERRED
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |t| t.checked_add(size).filter(|&t| t <= standard))
            .is_ok();
    moved.into()
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "xmem",
        dlls: &["xmem.dll", "kernel32.dll", "kernelbase.dll", "api-ms-win-core-memory-*"],
        exports: vec![
            hle_export!(VirtualAlloc),
            hle_export!(VirtualFree),
            hle_export!(VirtualQuery),
            hle_export!(VirtualQueryEx),
            hle_export!(XMemAlloc),
            hle_export!(XMemFree),
            hle_export!(XMemAllocDefault),
            hle_export!(XMemFreeDefault),
            hle_export!(XMemIsToolsMemAvailable),
            hle_export!(XMemAllocatePhysicalPages),
            hle_export!(XMemFreePhysicalPages),
            hle_export!(XMemMapPhysicalPages),
            hle_export!(XMemMapPhysicalPagesScatter),
            hle_export!(XMemVirtualAlloc),
            hle_export!(XMemVirtualQuery),
            hle_export!(XMemGetWorkingSetStatistics),
            hle_export!(XMemTransferMemory),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_commits_and_physical_aliases() {
        let at = XMemVirtualAlloc(std::ptr::null_mut(), 4 * PAGE, MEM_RESERVE | PAGES_64K, 0, PAGE_READWRITE) as usize;
        assert!(at >= 1 << 32 && at % PAGE == 0, "{at:#x}");
        let info = query(at);
        assert_eq!((info.state, info.allocation_base, info.region_size), (MEM_RESERVE, at, 4 * PAGE));
        let mut n = 1usize;
        let mut page = 0usize;
        assert_eq!(XMemAllocatePhysicalPages(0, &mut n, &mut page), 1);
        assert_eq!(page % FRAMES, 0);
        let pages = [page, page];
        assert!(!XMemMapPhysicalPages(at as *mut c_void, 1, &pages[0]).is_null());
        let addresses = [(at + 2 * PAGE) as *mut c_void];
        assert_eq!(XMemMapPhysicalPagesScatter(addresses.as_ptr(), 1, &pages[1]), 1);
        // SAFETY: both addresses show the same physical page.
        unsafe {
            ((at + 100) as *mut u32).write_volatile(0xfeed);
            assert_eq!(((at + 2 * PAGE + 100) as *const u32).read_volatile(), 0xfeed, "aliased");
        }
        assert_eq!(query(at).state, MEM_COMMIT);
        assert_eq!(query(at + PAGE).state, MEM_RESERVE);
        assert!(pages_known(&[page], 8, 16));
        assert!(write_physical(&[page], 8, b"streamed"));
        let mut window = page_window(&[page], 200, 4).expect("one frame");
        window.bytes().copy_from_slice(b"view");
        // SAFETY: the page is mapped at `at`.
        unsafe {
            assert_eq!(std::slice::from_raw_parts((at + 8) as *const u8, 8), b"streamed", "written where the title sees it");
            assert_eq!(std::slice::from_raw_parts((at + 200) as *const u8, 4), b"view");
        }
        assert!(!pages_known(&[page + 9999 * FRAMES], 0, 1));
        assert!(!XMemMapPhysicalPages(at as *mut c_void, 1, std::ptr::null()).is_null());
        assert_eq!(query(at).state, MEM_RESERVE, "unmapped");
        let low = XMemVirtualAlloc(std::ptr::null_mut(), 3 * SMALL, MEM_RESERVE | MEM_COMMIT, ADDRESS_32BIT, PAGE_READWRITE) as usize;
        assert!(low != 0 && low < 1 << 32, "{low:#x}");
        // SAFETY: committed read-write memory.
        unsafe { (low as *mut u64).write(1) };
        let committed = XMemVirtualAlloc(std::ptr::null_mut(), PAGE, MEM_COMMIT | PAGES_64K, 0, 0x42404) as usize;
        assert_ne!(committed, 0, "a commit without an address reserves too");
        assert_eq!(query(committed).state, MEM_COMMIT);
        let plain = VirtualAlloc(std::ptr::null_mut(), 8 * SMALL, MEM_RESERVE, PAGE_NOACCESS) as usize;
        assert_eq!(VirtualAlloc((plain + SMALL) as *mut c_void, SMALL, MEM_COMMIT, PAGE_READWRITE) as usize, plain + SMALL);
        assert_eq!((query(plain + SMALL).state, query(plain + SMALL).region_size), (MEM_COMMIT, SMALL));
        assert_eq!(VirtualFree(plain as *mut c_void, 0, MEM_RELEASE), 1);
        assert_eq!(query(plain).state, MEM_FREE);
        let stack = &n as *const usize as usize;
        assert_eq!(query(stack).state, MEM_COMMIT, "host memory: the stack");
    }
}
