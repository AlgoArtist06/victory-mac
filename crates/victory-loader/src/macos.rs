//! Mapping title images on macOS, where no PE loader exists: sections at
//! the preferred base when it is free (base relocations otherwise), static
//! TLS under indices we choose, and exports read from the export table.
//!
//! The executable gets TLS index 0, as Windows gives an exe, so its code
//! reaches its block through `gs:[0x58]` unpatched (see [`enter_thread`]).
//! On macOS `gs:` points at the pthread slots; slots 6 and 11 (offsets
//! 0x30 and 0x58) are left free by libSystem for this use.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, bail};
use victory_pe::{ImageInfo, ImportSymbol, base_relocations};

use crate::images::BindReport;
use crate::thunks::UnimplementedPolicy;
use crate::{ImportResolver, PreparedImage};

const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;
const PAGE: usize = 4096;

/// A title image mapped into this process. A clone names the same mapping.
#[derive(Debug, Clone)]
pub struct LoadedImage {
    base: usize,
    path: PathBuf,
    info: ImageInfo,
}

/// One image's static TLS: the template copied into each thread's block.
#[derive(Clone, Copy)]
struct TlsTemplate {
    start: usize,
    len: usize,
    zero_fill: usize,
}

/// Static TLS templates by index; index 0 is the executable's.
static TLS: Mutex<Vec<Option<TlsTemplate>>> = Mutex::new(Vec::new());

/// Maps `prepared` at its preferred base if that is free, else anywhere
/// with its base relocations applied. Runs no title code.
pub fn load(prepared: &PreparedImage, _dir: &Path) -> Result<LoadedImage> {
    let info = &prepared.info;
    let bytes = &prepared.bytes;
    let size = (info.size_of_image as usize).next_multiple_of(PAGE);
    let base = map_at(info.image_base as usize, size)?;
    let delta = base.wrapping_sub(info.image_base as usize);
    if delta != 0 && !info.has_relocations {
        bail!(
            "preferred base {:#x} is taken and the image has no relocations",
            info.image_base
        );
    }
    let image = base as *mut u8;
    let headers = info
        .sections
        .iter()
        .map(|s| s.rva as usize)
        .min()
        .unwrap_or(PAGE)
        .min(bytes.len());
    // SAFETY: `image` is a fresh writable mapping of `size` bytes, and every
    // copy below is bounded by both the file and the section's place in it.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), image, headers);
        for s in &info.sections {
            let (from, len) = (
                s.file_offset as usize,
                s.file_size.min(s.virtual_size) as usize,
            );
            let len = len
                .min(bytes.len().saturating_sub(from))
                .min(size.saturating_sub(s.rva as usize));
            std::ptr::copy_nonoverlapping(bytes.as_ptr().add(from), image.add(s.rva as usize), len);
        }
    }
    if delta != 0 {
        for rva in base_relocations(bytes)? {
            if rva as usize + 8 > size {
                bail!("base relocation at {rva:#x} is outside the image");
            }
            // SAFETY: inside the mapping (checked above); slots may be unaligned.
            unsafe {
                let slot = image.add(rva as usize).cast::<u64>();
                slot.write_unaligned(slot.read_unaligned().wrapping_add(delta as u64));
            }
        }
    }
    let loaded = LoadedImage {
        base,
        path: PathBuf::from(prepared.file_name()),
        info: info.clone(),
    };
    // SAFETY: just mapped and relocated, and no title code has run.
    unsafe { loaded.register_tls() };
    loaded.protect_sections()?;
    MAPPED.lock().unwrap_or_else(|e| e.into_inner()).push((base, size));
    tracing::info!(
        base = format_args!("{base:#x}"),
        preferred = format_args!("{:#x}", info.image_base),
        "image mapped"
    );
    Ok(loaded)
}

/// Every mapped image: base and size.
static MAPPED: Mutex<Vec<(usize, usize)>> = Mutex::new(Vec::new());

/// The base and size of the mapped image that holds `address`.
pub fn image_containing(address: usize) -> Option<(usize, usize)> {
    MAPPED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .copied()
        .find(|&(base, size)| (base..base + size).contains(&address))
}

/// The address of an export of the mapped image at `base`, by name or by
/// ordinal (`name` below 0x10000), from its export directory. Forwarded
/// exports are not followed.
///
/// # Safety
/// `base` is a mapped image, and a name is NUL-terminated.
pub unsafe fn export_of(base: usize, name: *const u8) -> Option<usize> {
    // SAFETY: the image's headers and export directory.
    unsafe {
        let nt = base + (base as *const u8).add(0x3c).cast::<u32>().read_unaligned() as usize;
        let dir = (nt + 0x18 + 112) as *const u32;
        let (rva, size) = (dir.read_unaligned() as usize, dir.add(1).read_unaligned() as usize);
        if rva == 0 {
            return None;
        }
        let at = |offset: usize| ((base + rva + offset) as *const u32).read_unaligned() as usize;
        let (ordinal_base, functions, names) = (at(0x10), at(0x14), at(0x18));
        let (function_table, name_table, ordinal_table) = (base + at(0x1c), base + at(0x20), base + at(0x24));
        let index = if (name as usize) < 0x1_0000 {
            (name as usize).checked_sub(ordinal_base)?
        } else {
            let wanted = std::ffi::CStr::from_ptr(name.cast());
            let i = (0..names).find(|&i| {
                let at = base + ((name_table + 4 * i) as *const u32).read_unaligned() as usize;
                std::ffi::CStr::from_ptr(at as *const std::ffi::c_char) == wanted
            })?;
            usize::from(((ordinal_table + 2 * i) as *const u16).read_unaligned())
        };
        if index >= functions {
            return None;
        }
        let function = ((function_table + 4 * index) as *const u32).read_unaligned() as usize;
        (function != 0 && !(rva..rva + size).contains(&function)).then_some(base + function)
    }
}

/// A fresh read+write mapping of `size` bytes at `want` if that is free.
fn map_at(want: usize, size: usize) -> Result<usize> {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANON;
    let prot = libc::PROT_READ | libc::PROT_WRITE;
    // SAFETY: without MAP_FIXED the kernel only takes `want` as a hint and
    // never replaces an existing mapping.
    let at = unsafe { libc::mmap(want as *mut c_void, size, prot, flags, -1, 0) };
    if at == libc::MAP_FAILED {
        bail!(
            "mmap of {size:#x} bytes failed: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(at as usize)
}

impl LoadedImage {
    pub fn base(&self) -> usize {
        self.base
    }

    /// Parsed from the original image (imports, entry point, TLS callbacks).
    pub fn info(&self) -> &ImageInfo {
        &self.info
    }

    /// The prepared image's file name, which names the build.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Address of an export of the image, if it has one by that name.
    pub fn export(&self, name: &str) -> Option<usize> {
        let export = self
            .info
            .exports
            .iter()
            .find(|e| e.name.as_deref() == Some(name))?;
        Some(self.base + export.rva? as usize)
    }

    /// Address of an export, by name or ordinal. Forwarded exports are not
    /// followed (title DLLs have none).
    pub fn export_symbol(&self, symbol: &ImportSymbol) -> Option<usize> {
        match symbol {
            ImportSymbol::Name { name, .. } => self.export(name),
            ImportSymbol::Ordinal(o) => {
                let export = self.info.exports.iter().find(|e| e.ordinal == *o)?;
                Some(self.base + export.rva? as usize)
            }
        }
    }

    /// Fills every IAT slot of the image.
    ///
    /// Each import goes to `resolver`; whatever it cannot resolve gets an
    /// unimplemented-import stub that behaves according to `policy`.
    pub fn bind_imports(
        &self,
        resolver: &dyn ImportResolver,
        policy: UnimplementedPolicy,
    ) -> Result<BindReport> {
        let (report, slots) = crate::images::resolve(&self.info, resolver, policy)?;
        for (rva, address) in slots {
            self.write(rva, address as u64)?;
        }
        tracing::info!(
            hle = report.hle,
            host = report.host,
            missing = report.missing.len(),
            "imports bound"
        );
        Ok(report)
    }

    /// The image's TLS index, if it has static TLS.
    pub fn tls_index(&self) -> Option<u32> {
        let tls = self.info.tls.as_ref()?;
        // SAFETY: the TLS directory and the index it names lie in the image.
        unsafe {
            let dir = (self.base + tls.directory_rva as usize) as *const u64;
            Some(((dir.add(2).read_unaligned()) as *const u32).read())
        }
    }

    /// Gives the image the next TLS index (0 for the executable) and keeps
    /// its template for the threads that enter later.
    ///
    /// # Safety
    /// Just mapped and relocated, still writable, and no title code ran.
    unsafe fn register_tls(&self) {
        let Some(tls) = &self.info.tls else { return };
        // SAFETY: IMAGE_TLS_DIRECTORY64 inside the image, already relocated:
        // start, end, address of index, callbacks (u64), zero fill (u32).
        let (start, end, index_at, zero_fill) = unsafe {
            let dir = (self.base + tls.directory_rva as usize) as *const u64;
            (
                dir.read_unaligned() as usize,
                dir.add(1).read_unaligned() as usize,
                dir.add(2).read_unaligned() as *mut u32,
                (dir.add(4) as *const u32).read_unaligned() as usize,
            )
        };
        let mut templates = TLS.lock().unwrap_or_else(|e| e.into_inner());
        let template = Some(TlsTemplate {
            start,
            len: end.saturating_sub(start),
            zero_fill,
        });
        let index = if self.info.is_dll() {
            let len = templates.len().max(1);
            templates.resize(len, None);
            templates.push(template);
            len
        } else {
            let len = templates.len().max(1);
            templates.resize(len, None);
            templates[0] = template;
            0
        };
        // SAFETY: the image's `_tls_index`, inside its writable mapping.
        unsafe { index_at.write(index as u32) };
    }

    /// Gives each section its protection from the section header.
    fn protect_sections(&self) -> Result<()> {
        for s in &self.info.sections {
            let at = self.base + s.rva as usize;
            let len = (s.virtual_size as usize).next_multiple_of(PAGE);
            // SAFETY: a section of our own mapping.
            if unsafe { libc::mprotect(at as *mut c_void, len, protection(s.characteristics)) } != 0
            {
                bail!(
                    "mprotect of section {} failed: {}",
                    s.name,
                    std::io::Error::last_os_error()
                );
            }
        }
        Ok(())
    }

    /// Writes `value` at `rva`, opening its page for the write.
    fn write(&self, rva: u32, value: u64) -> Result<()> {
        let at = self.base + rva as usize;
        let page = at & !(PAGE - 1);
        let span = (at + 8).next_multiple_of(PAGE) - page;
        let section = self
            .info
            .sections
            .iter()
            .find(|s| (s.rva..s.rva + s.virtual_size).contains(&rva));
        let restore = section.map_or(libc::PROT_READ, |s| protection(s.characteristics));
        // SAFETY: `at` lies in the image (the parser only yields slots there);
        // the pages are made writable for the write and restored after.
        unsafe {
            if libc::mprotect(
                page as *mut c_void,
                span,
                libc::PROT_READ | libc::PROT_WRITE,
            ) != 0
            {
                bail!(
                    "mprotect for IAT slot {rva:#x} failed: {}",
                    std::io::Error::last_os_error()
                );
            }
            (at as *mut u64).write_unaligned(value);
            libc::mprotect(page as *mut c_void, span, restore);
        }
        Ok(())
    }
}

fn protection(characteristics: u32) -> i32 {
    let mut prot = libc::PROT_READ;
    if characteristics & SCN_MEM_WRITE != 0 {
        prot |= libc::PROT_WRITE;
    }
    if characteristics & SCN_MEM_EXECUTE != 0 {
        prot |= libc::PROT_EXEC;
    }
    prot
}

/// Thread block offsets title code reads through `gs:[0x30]` (`NtCurrentTeb`).
mod teb {
    pub const STACK_BASE: usize = 0x08;
    pub const STACK_LIMIT: usize = 0x10;
    pub const SELF: usize = 0x30;
    pub const PROCESS_ID: usize = 0x40;
    pub const THREAD_ID: usize = 0x48;
    pub const TLS_ARRAY: usize = 0x58;
    pub const PEB: usize = 0x60;
    /// Large enough for every field Windows has.
    pub const SIZE: usize = 0x2000;
}

/// The process block (`PEB`) every thread block points at. Zeroed: title
/// code reads `BeingDebugged` from it, and finds no debugger.
fn peb() -> usize {
    static PEB: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *PEB.get_or_init(|| Box::leak(vec![0u8; 0x1000].into_boxed_slice()).as_mut_ptr() as usize)
}

/// Sets up the calling thread for title code: a thread block at `gs:[0x30]`,
/// and its static TLS blocks, each a copy of its image's template, in an
/// array at `gs:[0x58]` (also in the thread block).
/// ponytail: the blocks are leaked; freeing them at thread end comes with
/// the host layer's threads.
pub fn enter_thread() {
    let templates = TLS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let array: Vec<usize> = templates
        .iter()
        .map(|t| {
            let Some(t) = t else { return 0 };
            let mut block = vec![0u8; t.len + t.zero_fill].into_boxed_slice();
            // SAFETY: the template lies in a mapped image.
            unsafe {
                std::ptr::copy_nonoverlapping(t.start as *const u8, block.as_mut_ptr(), t.len)
            };
            Box::leak(block).as_mut_ptr() as usize
        })
        .collect();
    let array = Box::leak(array.into_boxed_slice()).as_ptr() as usize;
    let block = Box::leak(vec![0u8; teb::SIZE].into_boxed_slice()).as_mut_ptr() as usize;
    // SAFETY: plain queries about the calling thread.
    let (stack_base, stack_size, tid) = unsafe {
        let me = libc::pthread_self();
        let mut tid = 0u64;
        libc::pthread_threadid_np(me, &mut tid);
        (
            libc::pthread_get_stackaddr_np(me) as usize,
            libc::pthread_get_stacksize_np(me),
            tid,
        )
    };
    let fields = [
        (teb::STACK_BASE, stack_base),
        (teb::STACK_LIMIT, stack_base - stack_size),
        (teb::SELF, block),
        (teb::PROCESS_ID, std::process::id() as usize),
        (teb::THREAD_ID, thread_id(tid) as usize),
        (teb::TLS_ARRAY, array),
        (teb::PEB, peb()),
    ];
    for (offset, value) in fields {
        // SAFETY: inside the block just allocated.
        unsafe { ((block + offset) as *mut usize).write(value) };
    }
    // SAFETY: pthread slots 6 and 11 (gs:0x30, gs:0x58) are unused by libSystem.
    unsafe {
        std::arch::asm!(
            "mov gs:[0x30], {teb}",
            "mov gs:[0x58], {tls}",
            teb = in(reg) block,
            tls = in(reg) array,
            options(nostack),
        )
    };
}

/// Images attached so far, in order: each hears of every thread that starts.
static ATTACHED: Mutex<Vec<LoadedImage>> = Mutex::new(Vec::new());

/// Sets up a new title thread ([`enter_thread`]) and tells every attached
/// image of it (`DLL_THREAD_ATTACH`), as Windows does for `CreateThread`.
///
/// # Safety
/// Runs title code.
pub unsafe fn enter_title_thread() {
    const THREAD_ATTACH: u32 = 2;
    enter_thread();
    let images = ATTACHED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for image in &images {
        // SAFETY: forwarded from the caller.
        unsafe { image.notify(THREAD_ATTACH) };
    }
}

/// A Windows thread id (32 bits) for a macOS one. Windows ids are multiples
/// of 4, never 0.
pub fn thread_id(tid: u64) -> u32 {
    ((tid as u32) << 2).max(4)
}

impl LoadedImage {
    /// Runs the image's TLS callbacks with `reason`, then, if it is a DLL
    /// with an entry point, its `DllMain`, whose result is returned.
    ///
    /// # Safety
    /// Runs title code: imports must be bound, and the thread entered.
    pub unsafe fn notify(&self, reason: u32) -> Option<i32> {
        type Callback = unsafe extern "win64" fn(usize, u32, *mut c_void) -> i32;
        let call = |rva: u32| {
            // SAFETY: RVAs from the image's own headers; forwarded from the caller.
            unsafe {
                let f: Callback = std::mem::transmute(self.base + rva as usize);
                f(self.base, reason, std::ptr::null_mut())
            }
        };
        if let Some(tls) = &self.info.tls {
            tls.callback_rvas.iter().for_each(|&rva| {
                call(rva);
            });
        }
        (self.info.is_dll() && self.info.entry_rva != 0).then(|| call(self.info.entry_rva))
    }

    /// Starts the title on a new thread with the image's stack size: each DLL
    /// attached in order, then the executable's TLS callbacks and entry point.
    ///
    /// # Safety
    /// Runs title code. Imports must be bound.
    pub unsafe fn spawn_main(
        self: std::sync::Arc<Self>,
        dlls: Vec<(String, LoadedImage)>,
    ) -> Result<std::thread::JoinHandle<Result<u32>>> {
        const PROCESS_ATTACH: u32 = 1;
        if self.info.entry_rva == 0 {
            bail!("image has no entry point");
        }
        let stack = usize::try_from(self.info.stack_reserve)
            .unwrap_or(usize::MAX)
            .max(1 << 20);
        Ok(std::thread::Builder::new()
            .name("title-main".into())
            .stack_size(stack)
            .spawn(move || {
                enter_thread();
                for (name, dll) in &dlls {
                    // SAFETY: forwarded from the caller.
                    if unsafe { dll.notify(PROCESS_ATTACH) } == Some(0) {
                        bail!("DllMain of {name} returned FALSE");
                    }
                    ATTACHED.lock().unwrap_or_else(|e| e.into_inner()).push(dll.clone());
                }
                // SAFETY: forwarded from the caller.
                unsafe { self.notify(PROCESS_ATTACH) };
                ATTACHED.lock().unwrap_or_else(|e| e.into_inner()).push((*self).clone());
                type Entry = unsafe extern "win64" fn(usize) -> u32;
                // SAFETY: the entry RVA came from the image header.
                let entry: Entry =
                    unsafe { std::mem::transmute(self.base + self.info.entry_rva as usize) };
                // SAFETY: forwarded from the caller.
                Ok(unsafe { entry(peb()) })
            })?)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn protection_follows_the_section_flags() {
        use super::*;
        assert_eq!(protection(0x6000_0020), libc::PROT_READ | libc::PROT_EXEC);
        assert_eq!(protection(0xC000_0040), libc::PROT_READ | libc::PROT_WRITE);
        assert_eq!(protection(0x4000_0040), libc::PROT_READ);
    }
}
