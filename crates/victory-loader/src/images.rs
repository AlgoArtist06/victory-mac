//! What the Windows and macOS loaders share: finding the title's DLLs,
//! resolving imports, and the bind report.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::AtomicU64;

use anyhow::{Context, Result};
use victory_pe::{ImageInfo, ImportSymbol};

use crate::thunks::{ImportSite, ThunkBlock, UnimplementedPolicy};
use crate::{ImportResolver, LoadedImage, Provenance, Resolution, load, prepare};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingImport {
    pub dll: String,
    pub symbol: String,
}

/// Outcome of [`LoadedImage::bind_imports`].
#[derive(Debug, Default, Clone)]
pub struct BindReport {
    pub hle: usize,
    pub host: usize,
    pub title: usize,
    pub missing: Vec<MissingImport>,
}

/// Maps the DLLs the title ships in `title_dir` that `main` imports,
/// directly or through each other, in initialisation order (dependencies
/// first). Only DLL names `wanted` accepts are considered, so DLLs the host
/// or our runtime provide stay with them. Imports are not bound.
pub fn load_title_dlls(
    main: &ImageInfo,
    title_dir: &Path,
    cache_dir: &Path,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<Vec<(String, LoadedImage)>> {
    let mut loaded = Vec::new();
    let dirs = (title_dir, cache_dir);
    map_imports(main, dirs, wanted, &mut HashSet::new(), &mut loaded)?;
    Ok(loaded)
}

/// Maps the DLL `name` from `title_dir` like [`load_title_dlls`] does, with
/// the title DLLs it imports (dependencies first, `name` last); empty if
/// `wanted` refuses it or the folder has no such file. For DLLs a title
/// loads itself; `wanted` must refuse those already mapped.
pub fn load_title_dll(
    name: &str,
    title_dir: &Path,
    cache_dir: &Path,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<Vec<(String, LoadedImage)>> {
    let mut loaded = Vec::new();
    let dirs = (title_dir, cache_dir);
    map_dll(name, dirs, wanted, &mut HashSet::new(), &mut loaded)?;
    Ok(loaded)
}

fn map_imports(
    info: &ImageInfo,
    dirs: (&Path, &Path),
    wanted: &dyn Fn(&str) -> bool,
    seen: &mut HashSet<String>,
    loaded: &mut Vec<(String, LoadedImage)>,
) -> Result<()> {
    for module in info.imports.iter().chain(&info.delay_imports) {
        map_dll(&module.dll, dirs, wanted, seen, loaded)?;
    }
    Ok(())
}

fn map_dll(
    name: &str,
    dirs: (&Path, &Path),
    wanted: &dyn Fn(&str) -> bool,
    seen: &mut HashSet<String>,
    loaded: &mut Vec<(String, LoadedImage)>,
) -> Result<()> {
    let name = name.to_ascii_lowercase();
    let path = dirs.0.join(&name);
    if !seen.insert(name.clone()) || !wanted(&name) || !path.is_file() {
        return Ok(());
    }
    let original = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    let prepared = prepare(&original).with_context(|| format!("preparing {}", path.display()))?;
    let image = load(&prepared, dirs.1).with_context(|| format!("loading {}", path.display()))?;
    map_imports(image.info(), dirs, wanted, seen, loaded)?;
    loaded.push((name, image));
    Ok(())
}

/// Resolves through `inner`, then through the exports of the title's DLLs.
pub struct WithTitleDlls<'a> {
    pub inner: &'a dyn ImportResolver,
    pub dlls: &'a [(String, LoadedImage)],
}

impl ImportResolver for WithTitleDlls<'_> {
    fn resolve(&self, dll: &str, symbol: &ImportSymbol) -> Resolution {
        let found = self.inner.resolve(dll, symbol);
        if found != Resolution::Missing {
            return found;
        }
        self.dlls
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(dll))
            .and_then(|(_, image)| image.export_symbol(symbol))
            .map_or(Resolution::Missing, |address| Resolution::Found {
                address,
                provenance: Provenance::Title,
            })
    }
}

/// Resolves every import of `info` (delay-load ones too) to an address:
/// through `resolver`, else to a generated stub that behaves as `policy`
/// says. Returns the counts and each IAT slot's RVA with its address.
pub(crate) fn resolve(
    info: &ImageInfo,
    resolver: &dyn ImportResolver,
    policy: UnimplementedPolicy,
) -> Result<(BindReport, Vec<(u32, usize)>)> {
    let mut report = BindReport::default();
    let mut slots: Vec<(u32, usize)> = Vec::with_capacity(info.import_count());
    let mut unresolved: Vec<(u32, &str, &ImportSymbol)> = Vec::new();

    // Delay-load slots are bound eagerly too, so the title's own
    // delay-load helper (which would ask the host) never runs.
    for module in info.imports.iter().chain(&info.delay_imports) {
        for entry in &module.entries {
            match resolver.resolve(&module.dll, &entry.symbol) {
                Resolution::Found {
                    address,
                    provenance,
                } => {
                    match provenance {
                        Provenance::Hle => report.hle += 1,
                        Provenance::Host => report.host += 1,
                        Provenance::Title => report.title += 1,
                    }
                    slots.push((entry.iat_slot_rva, address));
                }
                Resolution::Missing => {
                    unresolved.push((entry.iat_slot_rva, &module.dll, &entry.symbol));
                }
            }
        }
    }

    if !unresolved.is_empty() {
        let mut block = ThunkBlock::new(unresolved.len())?;
        for (slot, dll, symbol) in unresolved {
            let site: &'static ImportSite = Box::leak(Box::new(ImportSite {
                dll: dll.to_owned(),
                symbol: symbol.to_string(),
                policy,
                calls: AtomicU64::new(0),
            }));
            slots.push((slot, block.emit(site)?));
            report.missing.push(MissingImport {
                dll: site.dll.clone(),
                symbol: site.symbol.clone(),
            });
        }
        block.seal()?;
    }

    Ok((report, slots))
}
