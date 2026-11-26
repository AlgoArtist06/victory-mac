//! Loads a title on macOS without running it: maps the executable and the
//! DLLs it ships, binds imports between them, and checks base relocations
//! by mapping the executable a second time elsewhere.
//!
//! `cargo run --target x86_64-apple-darwin -p victory-loader --example mac_load -- <title folder>`
//!
//! Imports our runtime or the host would provide are left to stubs here,
//! so each image's "missing" count is what the macOS port still has to
//! provide (the Wine run's runtime + host columns).

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use victory_loader::{
    ImportResolver, Resolution, UnimplementedPolicy, WithTitleDlls, load, load_title_dlls, prepare,
};
use victory_pe::{ImportSymbol, base_relocations};

struct Nothing;
impl ImportResolver for Nothing {
    fn resolve(&self, _: &str, _: &ImportSymbol) -> Resolution {
        Resolution::Missing
    }
}

fn main() -> Result<()> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("usage: mac_load <title folder>")?,
    );
    let exe = dir.join("game_scarlett_master.exe");
    let original = std::fs::read(&exe).with_context(|| format!("reading {}", exe.display()))?;
    let prepared = prepare(&original)?;
    let cache = std::env::temp_dir();
    let main = load(&prepared, &cache)?;
    println!(
        "mapped         {:#x} (preferred {:#x})",
        main.base(),
        main.info().image_base
    );

    // The host's own C++ runtime serves these on Windows (HostPolicy).
    // `MAC_LOAD_CXX=title`: map the C++ runtime the title ships instead.
    let cxx_from_title = std::env::var("MAC_LOAD_CXX").is_ok_and(|v| v == "title");
    let wanted = |dll: &str| {
        cxx_from_title || (!dll.starts_with("msvcp140") && !dll.starts_with("vcruntime140"))
    };
    let dlls = load_title_dlls(main.info(), &dir, &cache, &wanted)?;
    let resolver = WithTitleDlls {
        inner: &Nothing,
        dlls: &dlls,
    };
    // `MAC_LOAD_LIST=<file>`: every import left to provide, as `dll!symbol`.
    let mut list = Vec::new();
    for (name, dll) in &dlls {
        let report = dll.bind_imports(&resolver, UnimplementedPolicy::ReturnZero)?;
        list.extend(
            report
                .missing
                .iter()
                .map(|m| format!("{}!{}", m.dll, m.symbol)),
        );
        println!(
            "bound          {name} at {:#x}: {} title, {} to provide",
            dll.base(),
            report.title,
            report.missing.len()
        );
    }
    let report = main.bind_imports(&resolver, UnimplementedPolicy::ReturnZero)?;
    list.extend(
        report
            .missing
            .iter()
            .map(|m| format!("{}!{}", m.dll, m.symbol)),
    );
    if let Some(path) = std::env::var_os("MAC_LOAD_LIST") {
        list.sort();
        list.dedup();
        std::fs::write(&path, list.join("\n") + "\n")?;
    }
    println!(
        "bound          game_scarlett_master.exe: {} title, {} to provide",
        report.title,
        report.missing.len()
    );
    println!("tls index      {:?}", main.tls_index());
    ensure!(
        main.tls_index() == Some(0),
        "the executable has TLS index 0"
    );

    // A second copy cannot take the preferred base, so it is relocated:
    // every relocated slot moves by the bases' difference, nothing else does.
    let moved = load(&prepared, &cache)?;
    let delta = moved.base().wrapping_sub(main.base()) as u64;
    let relocs = base_relocations(&prepared.bytes)?;
    let sections = moved.info().sections.clone();
    let iat: std::collections::HashSet<u32> = main
        .info()
        .imports
        .iter()
        .chain(&main.info().delay_imports)
        .flat_map(|m| m.entries.iter().map(|e| e.iat_slot_rva))
        .collect();
    let mut checked = 0;
    for &rva in &relocs {
        if iat.contains(&rva) {
            continue;
        }
        // SAFETY: both images are mapped and readable at every relocation.
        let (a, b) = unsafe {
            (
                ((main.base() + rva as usize) as *const u64).read_unaligned(),
                ((moved.base() + rva as usize) as *const u64).read_unaligned(),
            )
        };
        ensure!(
            b.wrapping_sub(a) == delta,
            "relocation at {rva:#x}: {a:#x} vs {b:#x}"
        );
        checked += 1;
    }
    let readable = sections
        .iter()
        .filter(|s| s.characteristics & 0x4000_0000 != 0);
    let mut same = 0usize;
    for s in readable {
        let len = s.virtual_size as usize;
        // SAFETY: readable sections of both mappings.
        let (a, b) = unsafe {
            (
                std::slice::from_raw_parts((main.base() + s.rva as usize) as *const u8, len),
                std::slice::from_raw_parts((moved.base() + s.rva as usize) as *const u8, len),
            )
        };
        same += a.iter().zip(b).filter(|(x, y)| x == y).count();
    }
    println!(
        "relocated      second copy at {:#x}: {checked} of {} relocations moved by {delta:#x}",
        moved.base(),
        relocs.len()
    );
    println!("unmoved bytes  {same} identical between the copies");
    Ok(())
}
