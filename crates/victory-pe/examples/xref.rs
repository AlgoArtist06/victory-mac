//! Where a PE image's code refers to an address: RIP-relative operands
//! (a 32-bit displacement followed by up to 4 bytes of immediate) and
//! direct `call`/`jmp rel32`. Candidates by byte pattern, not disassembly:
//! check each with a disassembler.
//!
//! `cargo run -p victory-pe --example xref -- <image> <address in hex>...`

use victory_pe::ImageInfo;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: xref <image> <address>...")?;
    let targets: Vec<u64> = args
        .map(|a| u64::from_str_radix(a.trim_start_matches("0x"), 16))
        .collect::<std::result::Result<_, _>>()?;
    let data = std::fs::read(&path)?;
    let info = ImageInfo::parse(&data)?;
    for s in info.sections.iter().filter(|s| s.characteristics & 0x2000_0000 != 0) {
        let start = s.file_offset as usize;
        let code = &data[start..start + s.file_size as usize];
        let va = |i: usize| info.image_base + u64::from(s.rva) + i as u64;
        for i in 0..code.len().saturating_sub(4) {
            let disp = i64::from(i32::from_le_bytes(code[i..i + 4].try_into()?));
            for &target in &targets {
                // A displacement at `i`, the instruction ending 0-4 bytes after it.
                for imm in [0u64, 1, 2, 4] {
                    if va(i + 4).wrapping_add(imm).wrapping_add_signed(disp) == target {
                        println!("{:#x}: displacement at {:#x} (+{imm} immediate bytes) -> {target:#x}", va(i), va(i));
                    }
                }
                if i > 0 && matches!(code[i - 1], 0xe8 | 0xe9) && va(i + 4).wrapping_add_signed(disp) == target {
                    let what = if code[i - 1] == 0xe8 { "call" } else { "jmp" };
                    println!("{:#x}: {what} -> {target:#x}", va(i - 1));
                }
            }
        }
    }
    Ok(())
}
