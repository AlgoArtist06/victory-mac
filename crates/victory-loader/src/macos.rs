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
    tracing::info!(
        base = format_args!("{base:#x}"),
        preferred = format_args!("{:#x}", info.image_base),
        "image mapped"
    );
    Ok(loaded)
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

/// Sets up the calling thread for title code: its static TLS blocks, each
/// a copy of its image's template, in an array that `gs:[0x58]` points at.
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
    let array = Box::leak(array.into_boxed_slice()).as_ptr() as u64;
    // SAFETY: pthread slot 11 (gs:0x58) is unused by libSystem.
    unsafe { std::arch::asm!("mov gs:[0x58], {}", in(reg) array, options(nostack)) };
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
