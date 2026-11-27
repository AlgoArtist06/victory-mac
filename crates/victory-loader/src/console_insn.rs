//! Instructions the console's CPU (Zen 2) has and an Intel PC's lacks, emulated: the title's
//! code raises STATUS_ILLEGAL_INSTRUCTION on them (the title zeroes memory with a `clzero` loop at
//! its first frames), and the vectored handler steps over them here.
//!
//! Ported from XCL (github.com/ivall/xcl, GPL-2.0-only): `insn.rs` (decoder, as
//! `console_insn_decode.rs`, unchanged) and `eh.rs` `emulate_console_only`.

#[path = "console_insn_decode.rs"]
mod decode;

use decode::ConsoleOnly;
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
#[cfg(windows)]
use windows_sys::Win32::System::Diagnostics::Debug::CONTEXT;
#[cfg(not(windows))]
pub use os::CONTEXT;

const CONTINUE_EXECUTION: i32 = -1;

/// Whether what Windows said about memory is remembered per thread (the regions it found
/// writable, the code bytes it found readable) and not asked again at every emulated
/// instruction. A query walks the pages of the region it answers for: asked from outside, it
/// took 2-9 ms in the title's heaps of 0.6-2.2 GB and 89 ms in its 6.2 GB page pool (run 30),
/// and the title zeroes blocks of them with `clzero` loops all through a frame: six of its
/// threads spent 25-78 % of their time inside that query (runs 29 and 30), about 27 s of
/// processor time in Windows per 9 s frame. On by default; `VICTORY_CLZERO_ASK=1` asks every
/// time, as before.
fn remembers() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_CLZERO_ASK").is_none_or(|v| v != "1"))
}

/// Answers a remembered region gives before Windows is asked about it again: one freed or
/// protected meanwhile is forgotten then.
const ANSWERS: u32 = 4096;

thread_local! {
    /// The regions Windows last said are committed and writable (start, end), newest first,
    /// and the answers they gave since they were emptied.
    static WRITABLE: Cell<([(usize, usize); 4], u32)> = const { Cell::new(([(0, 0); 4], 0)) };
    /// Faulting addresses and the code bytes found readable there, newest first.
    static CODE: Cell<[(usize, usize); 8]> = const { Cell::new([(0, 0); 8]) };
}

/// Instructions emulated, the times Windows was asked about memory for them, and the bytes
/// `clzero` loops zeroed, since the start.
static EMULATED: AtomicU64 = AtomicU64::new(0);
static ASKED: AtomicU64 = AtomicU64::new(0);
static ZEROED: AtomicU64 = AtomicU64::new(0);

/// Whether `monitorx` and `mwaitx` are replaced in the title's code the first time they fault
/// (`monitorx` by a 3-byte no-op, `mwaitx` by `pause` and a no-op), so that they never fault
/// again. Emulated, each pass of such a wait loop is two trips into Windows and back, through
/// every vectored handler, and no lighter a wait for it: the emulation does no more than
/// `pause` either, and the loop looks at its value again after it. On by default;
/// `VICTORY_PATCH_WAITS=0` emulates each time, as before.
fn patches_waits() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PATCH_WAITS").is_none_or(|v| v != "0"))
}

/// What replaces the 3 bytes of a wait instruction.
fn wait_patch(what: ConsoleOnly) -> Option<[u8; 3]> {
    match what {
        ConsoleOnly::Monitorx => Some([0x0f, 0x1f, 0x00]),
        ConsoleOnly::Mwaitx => Some([0xf3, 0x90, 0x90]),
        _ => None,
    }
}

/// Whether a `clzero` loop ([`ConsoleOnly::ClzeroLoop`]) is replaced the first time it faults
/// by a jump to the same loop with plain stores ([`clzero_stub`]). Run cpu-5 (the dorm): 25,000
/// exceptions a second, 2,100 a frame, at five such loops of one function of the title, for
/// some kilobytes each; ten times what the wait instructions raised. On by default;
/// `VICTORY_PATCH_CLZERO=0` emulates each time, as before.
fn patches_clzero() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PATCH_CLZERO").is_none_or(|v| v != "0"))
}

/// `jmp rel32` at `at` to `to`, if it reaches.
pub(crate) fn jump(at: usize, to: usize) -> Option<[u8; 5]> {
    let rel = i32::try_from(to as i64 - (at as i64 + 5)).ok()?;
    let [a, b, c, d] = rel.to_le_bytes();
    Some([0xe9, a, b, c, d])
}

/// The `clzero` loop of 12 bytes at `at` with plain stores, as code at `stub`: every register
/// and the flags come out as the loop leaves them. `cmp` is its `cmp rax, reg` (3 bytes, the
/// register not `rax`), `None` for another form. (The scratch register is saved on the stack
/// and back before the compare, whose register it may be.)
fn clzero_stub(at: usize, stub: usize, cmp: [u8; 3]) -> Option<Vec<u8>> {
    if !matches!(cmp[0], 0x48 | 0x4c)
        || cmp[1] != 0x39
        || cmp[2] & 0xc7 != 0xc0
        || (cmp[0] == 0x48 && cmp[2] == 0xc0)
    {
        return None;
    }
    let mut code = vec![
        0x51, // push rcx
        0x48, 0x89, 0xc1, // mov rcx, rax
        0x48, 0x83, 0xe1, 0xc0, // and rcx, -64
    ];
    for k in 0..8 {
        code.extend([0x48, 0xc7, 0x41, k * 8, 0, 0, 0, 0]); // mov qword [rcx + 8k], 0
    }
    code.push(0x59); // pop rcx
    code.extend([0x48, 0x83, 0xc0, 0x40]); // add rax, 0x40
    code.extend(cmp);
    let back = -(code.len() as i64 + 2);
    code.extend([0x72, i8::try_from(back).ok()? as u8]); // jb the start
    let end = stub + code.len();
    code.extend(jump(end, at + 12)?);
    Some(code)
}

/// The status flags `cmp a, b` leaves (CF, PF, AF, ZF, SF, OF) as `EFLAGS` bits.
pub(crate) fn cmp_flags(a: u64, b: u64) -> u32 {
    let r = a.wrapping_sub(b);
    u32::from(a < b)
        | u32::from((r as u8).count_ones().is_multiple_of(2)) << 2
        | (((a ^ b ^ r) >> 4) as u32 & 1) << 4
        | u32::from(r == 0) << 6
        | ((r >> 63) as u32) << 7
        | ((((a ^ b) & (a ^ r)) >> 63) as u32) << 11
}
pub(crate) const CMP_FLAGS: u32 = 0x8d5;

/// Room for [`clzero_stub`]s near the title's code: where it is and the bytes used.
static STUBS: std::sync::Mutex<Option<(usize, usize)>> = std::sync::Mutex::new(None);
pub(crate) const STUBS_LEN: usize = 64 << 10;

/// Memory for code within reach of a `jmp rel32` at `at`.
pub(crate) fn near(at: usize) -> Option<usize> {
    let at = at & !0xffff;
    (0x10_0000..0x7000_0000)
        .step_by(0x10_0000)
        .flat_map(|d| [at.checked_sub(d), at.checked_add(d)])
        .flatten()
        .find_map(|place| os::code_at(place, STUBS_LEN))
}

/// Replaces the `clzero` loop at `at`, whose 12 bytes are `code`: whether it did.
///
/// No thread is ever inside the loop's other instructions (the emulation does the whole loop
/// in one), so the five bytes of the jump may take the place of two of them.
///
/// # Safety
/// `at` is in the title's code, which stays mapped.
unsafe fn patch_clzero(at: usize, code: &[u8]) -> bool {
    let &[a, b, c, d, e, _, _, x, y, z, 0x72, 0xf4] = code else {
        return false;
    };
    let mut stubs = STUBS.lock().unwrap_or_else(|e| e.into_inner());
    if stubs.is_none() {
        *stubs = near(at).map(|place| (place, 0));
    }
    let Some((place, used)) = stubs.as_mut() else {
        return false;
    };
    let stub = *place + *used;
    let (Some(body), Some(jump)) = (clzero_stub(at, stub, [x, y, z]), jump(at, stub)) else {
        return false;
    };
    if *used + body.len() > STUBS_LEN || (at & 15) + 5 > 16 {
        return false;
    }
    // SAFETY: inside the memory made for it, which nothing runs yet; then the title's code.
    unsafe {
        std::ptr::copy_nonoverlapping(body.as_ptr(), stub as *mut u8, body.len());
        *used += body.len().next_multiple_of(16);
        patch(at, [a, b, c, d, e], jump)
    }
}

/// The aligned 16 bytes `word` around address `at` with the bytes at `at` replaced; `None`
/// where they do not all lie in those 16. (16 and not 8: the title's busiest one lies over the
/// end of its 8 and took 28,000 exceptions a second, run cpu-6.)
fn with_patch(word: u128, at: usize, patch: &[u8]) -> Option<u128> {
    let offset = at & 15;
    if offset + patch.len() > 16 {
        return None;
    }
    let mut bytes = word.to_le_bytes();
    bytes[offset..offset + patch.len()].copy_from_slice(patch);
    Some(u128::from_le_bytes(bytes))
}

/// One thread at a time changes code: a second one would take the first one's loosened
/// protection for the page's own.
static PATCHING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Replaces the bytes at `at`, which are `was`, by `patch`, in one store that other threads
/// see whole: whether they are replaced now (not where they cross 16 aligned bytes, or the page
/// cannot be written).
///
/// # Safety
/// `at` is in code that stays mapped, and `patch` does for it what `was` does.
unsafe fn patch<const N: usize>(at: usize, was: [u8; N], patch: [u8; N]) -> bool {
    use std::sync::atomic::Ordering::SeqCst;
    if (at & 15) + N > 16 || !std::is_x86_feature_detected!("cmpxchg16b") {
        return false;
    }
    let _one = PATCHING.lock().unwrap_or_else(|e| e.into_inner());
    let word = (at & !15) as *mut u128;
    let here = (at & 15)..(at & 15) + N;
    // SAFETY: the aligned 16 bytes around the instruction lie in one page of mapped code, made
    // writable for the store and put back; the processor has the instruction (asked above).
    unsafe {
        let Some(old) = os::open_code(word as usize, 16) else {
            return false;
        };
        // What is there (zeros are replaced by zeros).
        let mut now = std::arch::x86_64::cmpxchg16b(word, 0, 0, SeqCst, SeqCst);
        let done = loop {
            let bytes = &now.to_le_bytes()[here.clone()];
            if bytes == patch {
                // (Another thread's fault at the same place: done already.)
                break true;
            }
            let Some(new) = (bytes == was)
                .then(|| with_patch(now, at, &patch))
                .flatten()
            else {
                break false;
            };
            let there = std::arch::x86_64::cmpxchg16b(word, now, new, SeqCst, SeqCst);
            if there == now {
                break true;
            }
            now = there;
        };
        os::close_code(word as usize, 16, old);
        done
    }
}

/// Diagnostics: how often each place in the title's code raises one of these exceptions, the
/// busiest logged every five seconds.
#[cfg(feature = "diagnostics")]
mod sites {
    use super::{AtomicU64, Relaxed};
    use std::sync::atomic::AtomicUsize;

    /// Address, what is there (an index into [`NAMES`]), exceptions since the last line.
    static SITES: [(AtomicUsize, AtomicUsize, AtomicU64); 512] =
        [const { (AtomicUsize::new(0), AtomicUsize::new(0), AtomicU64::new(0)) }; 512];
    const NAMES: [&str; 8] = [
        "other",
        "clwb",
        "monitorx",
        "mwaitx",
        "clzero",
        "clzero loop",
        "rdpru",
        "extrq/insertq",
    ];
    static LAST: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    static PATCHED: AtomicU64 = AtomicU64::new(0);

    pub(super) fn patched() {
        PATCHED.fetch_add(1, Relaxed);
    }

    pub(super) fn count(at: usize, what: super::ConsoleOnly) {
        use super::ConsoleOnly as C;
        let kind = match what {
            C::Clwb => 1,
            C::Monitorx => 2,
            C::Mwaitx => 3,
            C::Clzero => 4,
            C::ClzeroLoop { .. } => 5,
            C::Rdpru => 6,
            C::Extrq { .. } | C::Insertq { .. } => 7,
            _ => 0,
        };
        let mut i = (at ^ at >> 9) % SITES.len();
        for _ in 0..8 {
            let s = &SITES[i];
            let there =
                s.0.compare_exchange(0, at, Relaxed, Relaxed)
                    .unwrap_or_else(|there| there);
            if there == 0 || there == at {
                s.1.store(kind, Relaxed);
                s.2.fetch_add(1, Relaxed);
                break;
            }
            i = (i + 1) % SITES.len();
        }
        // Now and then: is a line due? Never waited for.
        if let Ok(mut last) = LAST.try_lock() {
            let since = *last.get_or_insert_with(std::time::Instant::now);
            if since.elapsed().as_secs() >= 5 {
                let s = since.elapsed().as_secs_f64();
                *last = Some(std::time::Instant::now());
                drop(last);
                line(s);
            }
        }
    }

    fn line(seconds: f64) {
        let mut all: Vec<(u64, usize, usize)> = SITES
            .iter()
            .map(|s| (s.2.swap(0, Relaxed), s.0.load(Relaxed), s.1.load(Relaxed)))
            .filter(|s| s.0 > 0)
            .collect();
        all.sort_unstable_by_key(|s| std::cmp::Reverse(s.0));
        let mut kinds = [0u64; NAMES.len()];
        for s in &all {
            kinds[s.2] += s.0;
        }
        let kinds: Vec<String> = NAMES
            .iter()
            .zip(kinds)
            .filter(|(_, n)| *n > 0)
            .map(|(name, n)| format!("{name} {:.0}", n as f64 / seconds))
            .collect();
        let top: Vec<String> = all
            .iter()
            .take(8)
            .map(|s| format!("{:#x} {} {:.0}", s.1, NAMES[s.2], s.0 as f64 / seconds))
            .collect();
        tracing::info!(
            "console CPU instructions that fault, a second: {}; places replaced so far: {}; the busiest places: {}",
            kinds.join(", "),
            PATCHED.load(Relaxed),
            top.join(", ")
        );
    }
}

fn readable(va: usize, bytes: usize) -> bool {
    ASKED.fetch_add(1, Relaxed);
    os::region(va).is_some_and(|(_, end, read, _)| read && end >= va + bytes)
}

fn writable(va: usize, bytes: usize) -> bool {
    writable_to(va, va + bytes) == Some(va + bytes)
}

/// Where the committed, writable memory that `va` lies in ends, at most `end`; None if `va`
/// is not in such memory. A region found is remembered ([`remembers`]).
/// shortcut: a remembered region that was freed or protected since is written all the same
/// for up to [`ANSWERS`] answers: the write then faults inside the handler, which the guard
/// on view memory opens as it does any write, and which is a crash otherwise (as the
/// unchecked single-line emulation behind this one already is).
fn writable_to(va: usize, end: usize) -> Option<usize> {
    let remember = remembers();
    if remember {
        let (regions, answers) = WRITABLE.with(Cell::get);
        if let Some(&(_, to)) = regions.iter().find(|&&(from, to)| from <= va && va < to) {
            let next = if answers >= ANSWERS {
                ([(0, 0); 4], 0)
            } else {
                (regions, answers + 1)
            };
            WRITABLE.with(|w| w.set(next));
            return Some(to.min(end));
        }
    }
    ASKED.fetch_add(1, Relaxed);
    let (from, to) = match os::region(va) {
        Some((from, to, _, true)) => (from, to),
        _ => return None,
    };
    if remember {
        WRITABLE.with(|w| {
            let (mut regions, answers) = w.get();
            regions.rotate_right(1);
            regions[0] = (from, to);
            w.set((regions, answers));
        });
    }
    Some(to.min(end))
}

/// An illegal instruction at `ctx.Rip`: a console-only one is emulated and stepped over
/// (`CONTINUE_EXECUTION`); anything else returns 0 (the crash report and the title's handlers
/// see it).
///
/// # Safety
/// `ctx` is the faulting thread's context, from the exception being dispatched.
pub(crate) unsafe fn emulate(ctx: &mut CONTEXT) -> i32 {
    let rip = ctx.Rip as usize;
    // The same few instructions fault again and again: their bytes are asked about once.
    let known = if remembers() {
        CODE.with(Cell::get)
    } else {
        [(0, 0); 8]
    };
    let n = match known.iter().find(|&&(at, n)| at == rip && n > 0) {
        Some(&(_, n)) => n,
        None => {
            if !readable(rip, 1) {
                return 0;
            }
            let n = (1..=16).rev().find(|&n| readable(rip, n)).unwrap_or(0);
            if remembers() {
                let mut known = known;
                known.rotate_right(1);
                known[0] = (rip, n);
                CODE.with(|c| c.set(known));
            }
            n
        }
    };
    // SAFETY: `n` bytes at `rip` are readable.
    let code = unsafe { std::slice::from_raw_parts(rip as *const u8, n) };
    let Some((what, len)) = decode::decode(code) else {
        return 0;
    };
    static SEEN: std::sync::Mutex<Vec<std::mem::Discriminant<ConsoleOnly>>> =
        std::sync::Mutex::new(Vec::new());
    if let Ok(mut seen) = SEEN.lock()
        && !seen.contains(&std::mem::discriminant(&what))
    {
        seen.push(std::mem::discriminant(&what));
        if what != ConsoleOnly::Ud2 {
            tracing::warn!(instruction = ?what, at = format_args!("{rip:#x}"), "a console CPU instruction this CPU lacks: emulated (logged once)");
        }
    }
    #[cfg(feature = "diagnostics")]
    if what != ConsoleOnly::Ud2 {
        sites::count(rip, what);
    }
    match what {
        ConsoleOnly::Ud2 => return 0,
        ConsoleOnly::Clwb => {
            // nothing to write back on the PC: patched to NOPs, no exception next time
            let _one = PATCHING.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: the instruction's own bytes, made writable for the patch and restored.
            unsafe {
                if let Some(old) = os::open_code(rip, len) {
                    std::slice::from_raw_parts_mut(rip as *mut u8, len).fill(0x90);
                    os::close_code(rip, len, old);
                }
            }
        }
        ConsoleOnly::Monitorx | ConsoleOnly::Mwaitx => {
            // Without a prefix, so 3 bytes: replaced, the next pass does not fault.
            if len == 3
                && patches_waits()
                && let Some(new) = wait_patch(what)
                // SAFETY: the title's code, where the instruction just faulted; both
                // replacements are waits that wait for nothing, as the emulation is.
                && unsafe { patch(rip, [code[0], code[1], code[2]], new) }
            {
                #[cfg(feature = "diagnostics")]
                sites::patched();
            }
            std::hint::spin_loop();
        }
        ConsoleOnly::Clzero => {
            // with an address-size prefix (0x67) the line is at EAX, zero-extended
            let a32 = code[..len - 3].contains(&0x67);
            let line = (if a32 { ctx.Rax & 0xFFFF_FFFF } else { ctx.Rax } as usize) & !63;
            if !writable(line, 64) {
                return 0; // the real instruction faults there too
            }
            // SAFETY: the 64-byte line is committed and writable.
            unsafe { std::ptr::write_bytes(line as *mut u8, 0, 64) };
        }
        ConsoleOnly::ClzeroLoop { end_reg } => {
            // the whole `clzero; add rax,40h; cmp rax,end; jb` loop at once
            // (and not again: the next time it is plain stores)
            if len == 12
                && patches_clzero()
                // SAFETY: the title's code, where the loop's first instruction just faulted.
                && unsafe { patch_clzero(rip, &code[..12]) }
            {
                #[cfg(feature = "diagnostics")]
                sites::patched();
            }
            let regs: [u64; 16] = [
                ctx.Rax, ctx.Rcx, ctx.Rdx, ctx.Rbx, ctx.Rsp, ctx.Rbp, ctx.Rsi, ctx.Rdi, ctx.R8,
                ctx.R9, ctx.R10, ctx.R11, ctx.R12, ctx.R13, ctx.R14, ctx.R15,
            ];
            let end = regs[end_reg as usize & 15] as usize;
            let start = ctx.Rax as usize;
            // A pass zeroes the whole line `rax` is in, wherever in it, and the test comes
            // after it: lines are zeroed past an `end` inside one, and one is for an `end`
            // not above the start. (The bytes from `start` to `end` were, `rax` came out as
            // `end`, and the flags stayed what they were.)
            const MOST: usize = (2 << 30) / 64;
            let wanted = if end > start {
                (end - start).div_ceil(64)
            } else {
                1
            };
            let passes = wanted.min(MOST);
            let first = start & !63;
            let Some(stop) = first.checked_add(passes * 64) else {
                return 0;
            };
            // Region by region: a block may lie across several (the title's pages are
            // mapped one view each), and one answer for all of it was a refusal, after
            // which every 64-byte line took an exception of its own.
            let mut at = first;
            while at < stop {
                let Some(to) = writable_to(at, stop) else {
                    break;
                };
                // SAFETY: committed and writable from `at` to `to`.
                unsafe { std::ptr::write_bytes(at as *mut u8, 0, to - at) };
                at = to;
            }
            let done = (at - first) / 64;
            if done == 0 {
                return 0;
            }
            ZEROED.fetch_add((done * 64) as u64, Relaxed);
            ctx.Rax = ctx.Rax.wrapping_add((done * 64) as u64);
            if done < wanted {
                // What follows is not writable (or the block is huge): the loop goes on
                // there (the same instruction again), where the real one faults too.
                return CONTINUE_EXECUTION;
            }
            ctx.EFlags = ctx.EFlags & !CMP_FLAGS
                | cmp_flags(ctx.Rax, end as u64);
        }
        ConsoleOnly::Rdpru => {
            let v = os::counter();
            let v = if ctx.Rcx as u32 <= 1 { v } else { 0 };
            ctx.Rax = v & 0xFFFF_FFFF;
            ctx.Rdx = v >> 32;
        }
        ConsoleOnly::Rdpid(r) => {
            let cpu = os::processor();
            let regs: [&mut u64; 16] = [
                &mut ctx.Rax,
                &mut ctx.Rcx,
                &mut ctx.Rdx,
                &mut ctx.Rbx,
                &mut ctx.Rsp,
                &mut ctx.Rbp,
                &mut ctx.Rsi,
                &mut ctx.Rdi,
                &mut ctx.R8,
                &mut ctx.R9,
                &mut ctx.R10,
                &mut ctx.R11,
                &mut ctx.R12,
                &mut ctx.R13,
                &mut ctx.R14,
                &mut ctx.R15,
            ];
            *regs[r as usize & 15] = cpu;
        }
        // the low quadword changes; the high one is zeroed, as a Zen 5 does
        ConsoleOnly::Extrq { dst, src, len, idx } => {
            // SAFETY: the context's XMM registers (CONTEXT_FLOATING_POINT is captured for exceptions).
            let x = unsafe { &mut ctx.Anonymous.FltSave.XmmRegisters };
            let (len, idx) = match src {
                Some(s) => {
                    let c = x[s as usize & 15].Low;
                    (c as u8, (c >> 8) as u8)
                }
                None => (len, idx),
            };
            let d = &mut x[dst as usize & 15];
            d.Low = decode::extrq(d.Low, len, idx);
            d.High = 0;
        }
        ConsoleOnly::Insertq {
            dst,
            src,
            reg,
            len,
            idx,
        } => {
            // SAFETY: as above.
            let x = unsafe { &mut ctx.Anonymous.FltSave.XmmRegisters };
            let s = x[src as usize & 15];
            let (len, idx) = if reg {
                (s.High as u8, (s.High >> 8) as u8)
            } else {
                (len, idx)
            };
            let d = &mut x[dst as usize & 15];
            d.Low = decode::insertq(d.Low, s.Low, len, idx);
            d.High = 0;
        }
    }
    ctx.Rip += len as u64;
    // How often this runs at all (each one is an exception: a trip into Windows and back).
    let emulated = EMULATED.fetch_add(1, Relaxed) + 1;
    if emulated >= 1 << 16 && emulated.is_power_of_two() {
        tracing::info!(
            emulated,
            asked = ASKED.load(Relaxed),
            zeroed_mb = ZEROED.load(Relaxed) >> 20,
            "console CPU instructions emulated so far, the times Windows was asked about memory for them, and what clzero loops zeroed"
        );
    }
    CONTINUE_EXECUTION
}

/// What the emulation asks of the system.
#[cfg(windows)]
mod os {
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READWRITE, PAGE_GUARD,
        PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc, VirtualProtect, VirtualQuery,
    };

    /// `len` bytes of code memory at exactly `at`, if free.
    pub(super) fn code_at(at: usize, len: usize) -> Option<usize> {
        // SAFETY: reserves free memory at that address or fails.
        let got = unsafe {
            VirtualAlloc(at as *const _, len, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE)
        } as usize;
        (got != 0).then_some(got)
    }

    /// Makes code writable; its old protection.
    ///
    /// # Safety
    /// `at..at + len` is mapped code.
    pub(super) unsafe fn open_code(at: usize, len: usize) -> Option<u32> {
        let mut old = 0u32;
        // SAFETY: guaranteed by the caller.
        (unsafe { VirtualProtect(at as *const _, len, PAGE_EXECUTE_READWRITE, &mut old) } != 0)
            .then_some(old)
    }

    /// # Safety
    /// As [`open_code`], with its answer.
    pub(super) unsafe fn close_code(at: usize, len: usize, old: u32) {
        use windows_sys::Win32::System::Diagnostics::Debug::FlushInstructionCache;
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let mut was = 0u32;
        // SAFETY: guaranteed by the caller.
        unsafe {
            VirtualProtect(at as *const _, len, old, &mut was);
            FlushInstructionCache(GetCurrentProcess(), at as *const _, len);
        }
    }

    /// The committed region `va` is in: start, end, readable, writable.
    pub(super) fn region(va: usize) -> Option<(usize, usize, bool, bool)> {
        // SAFETY: plain data filled in by the query.
        let mut m: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: a valid out pointer of the right size.
        let found = unsafe { VirtualQuery(va as *const _, &mut m, size_of_val(&m)) } != 0;
        if !found || m.State != MEM_COMMIT {
            return None;
        }
        let from = m.BaseAddress as usize;
        Some((
            from,
            from + m.RegionSize,
            m.Protect & (PAGE_NOACCESS | PAGE_GUARD) == 0,
            m.Protect & (PAGE_READWRITE | PAGE_EXECUTE_READWRITE) != 0,
        ))
    }

    pub(super) fn counter() -> u64 {
        let mut v = 0i64;
        // SAFETY: a valid out pointer.
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut v) };
        v as u64
    }

    pub(super) fn processor() -> u64 {
        // SAFETY: no preconditions.
        u64::from(unsafe { windows_sys::Win32::System::Threading::GetCurrentProcessorNumber() })
    }
}

#[cfg(not(windows))]
mod os {
    /// The Windows `CONTEXT` the fault dispatcher hands over (same layout,
    /// the fields the emulation uses named as in `windows_sys`).
    #[allow(non_snake_case)]
    #[repr(C, align(16))]
    pub struct CONTEXT {
        _home: [u64; 6],
        pub ContextFlags: u32,
        pub MxCsr: u32,
        _segments: [u16; 6],
        pub EFlags: u32,
        _debug: [u64; 6],
        pub Rax: u64,
        pub Rcx: u64,
        pub Rdx: u64,
        pub Rbx: u64,
        pub Rsp: u64,
        pub Rbp: u64,
        pub Rsi: u64,
        pub Rdi: u64,
        pub R8: u64,
        pub R9: u64,
        pub R10: u64,
        pub R11: u64,
        pub R12: u64,
        pub R13: u64,
        pub R14: u64,
        pub R15: u64,
        pub Rip: u64,
        pub Anonymous: FloatArea,
        _rest: [u8; 0x4d0 - 0x300],
    }
    const _: () = assert!(size_of::<CONTEXT>() == 0x4d0);

    #[allow(non_snake_case)]
    #[repr(C)]
    pub struct FloatArea {
        pub FltSave: FloatSave,
    }

    #[allow(non_snake_case)]
    #[repr(C)]
    pub struct FloatSave {
        _header: [u8; 0xa0],
        pub XmmRegisters: [M128A; 16],
        _reserved: [u8; 96],
    }

    #[allow(non_snake_case)]
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct M128A {
        pub Low: u64,
        pub High: i64,
    }

    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_vm_allocate(task: u32, address: *mut u64, size: u64, flags: i32) -> i32;
        fn mach_vm_region(task: u32, address: *mut u64, size: *mut u64, flavor: i32, info: *mut i32, count: *mut u32, object: *mut u32) -> i32;
        fn clock_gettime_nsec_np(clock: libc::clockid_t) -> u64;
    }

    const RWX: i32 = libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC;

    pub(super) fn code_at(at: usize, len: usize) -> Option<usize> {
        let mut address = at as u64;
        // SAFETY: a fixed allocation that fails rather than replace.
        unsafe {
            (mach_vm_allocate(mach_task_self_, &mut address, len as u64, 0) == 0
                && libc::mprotect(at as *mut _, len, RWX) == 0)
                .then_some(at)
        }
    }

    /// Makes code writable (its pages); title code is read and execute.
    ///
    /// # Safety
    /// `at..at + len` is mapped code.
    pub(super) unsafe fn open_code(at: usize, len: usize) -> Option<i32> {
        let page = at & !4095;
        let span = (at + len).next_multiple_of(4096) - page;
        // SAFETY: guaranteed by the caller.
        (unsafe { libc::mprotect(page as *mut _, span, RWX) } == 0).then_some(libc::PROT_READ | libc::PROT_EXEC)
    }

    /// # Safety
    /// As [`open_code`], with its answer.
    pub(super) unsafe fn close_code(at: usize, len: usize, old: i32) {
        let page = at & !4095;
        let span = (at + len).next_multiple_of(4096) - page;
        // SAFETY: guaranteed by the caller.
        unsafe { libc::mprotect(page as *mut _, span, old) };
    }

    pub(super) fn region(va: usize) -> Option<(usize, usize, bool, bool)> {
        let (mut address, mut size) = (va as u64, 0u64);
        let mut info = [0i32; 16];
        let mut count = 9u32;
        let mut object = 0u32;
        // SAFETY: a query into our buffers (VM_REGION_BASIC_INFO_64: protection first).
        let found = unsafe { mach_vm_region(mach_task_self_, &mut address, &mut size, 9, info.as_mut_ptr(), &mut count, &mut object) } == 0;
        (found && address as usize <= va)
            .then(|| (address as usize, (address + size) as usize, info[0] & 1 != 0, info[0] & 2 != 0))
    }

    /// 100 ns ticks, as Windows' counter.
    pub(super) fn counter() -> u64 {
        // SAFETY: a plain query.
        unsafe { clock_gettime_nsec_np(libc::CLOCK_MONOTONIC_RAW) / 100 }
    }

    pub(super) fn processor() -> u64 {
        0
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn wait_instructions_are_replaced_inside_sixteen_aligned_bytes() {
        let word = u128::from_le_bytes([
            1, 2, 3, 4, 5, 6, 7, 0x0f, 0x01, 0xfb, 11, 12, 13, 14, 15, 16,
        ]);
        let new = with_patch(word, 0x1007, &[0xf3, 0x90, 0x90]).unwrap();
        assert_eq!(
            new.to_le_bytes(),
            [
                1, 2, 3, 4, 5, 6, 7, 0xf3, 0x90, 0x90, 11, 12, 13, 14, 15, 16
            ]
        );
        assert!(with_patch(word, 0x100d, &[0; 3]).is_some());
        assert!(
            with_patch(word, 0x100e, &[0; 3]).is_none(),
            "crosses into the next sixteen"
        );
    }

    /// `monitorx; mwaitx; ret` in a page of its own: each is replaced at its first fault,
    /// and the page is protected as before.
    #[test]
    fn a_wait_faults_once_and_the_page_keeps_its_protection() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, MEMORY_BASIC_INFORMATION, PAGE_EXECUTE_READ,
            PAGE_READWRITE, VirtualAlloc, VirtualFree, VirtualProtect, VirtualQuery,
        };
        // SAFETY: memory of our own, read, written and run within its page.
        unsafe {
            let page = VirtualAlloc(
                std::ptr::null(),
                4096,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
            .cast::<u8>();
            assert!(!page.is_null());
            let code = [0x0f, 0x01, 0xfa, 0x0f, 0x01, 0xfb, 0xc3];
            // The first over the end of its aligned eight bytes, as the title's is; and one
            // over the end of its sixteen (at 46), which is left alone.
            std::ptr::copy_nonoverlapping(code.as_ptr(), page.add(6), code.len());
            std::ptr::copy_nonoverlapping(code.as_ptr(), page.add(46), code.len());
            let mut old = 0;
            assert_ne!(
                VirtualProtect(page.cast(), 4096, PAGE_EXECUTE_READ, &mut old),
                0
            );
            let at = page as usize;
            let mut context: CONTEXT = std::mem::zeroed();
            for rip in [at + 6, at + 9, at + 46] {
                context.Rip = rip as u64;
                assert_eq!(emulate(&mut context), CONTINUE_EXECUTION);
                assert_eq!(context.Rip, rip as u64 + 3);
            }
            if patches_waits() {
                let now = std::slice::from_raw_parts(page.add(6), 7);
                assert_eq!(now, [0x0f, 0x1f, 0x00, 0xf3, 0x90, 0x90, 0xc3]);
                // They run now.
                let run: extern "win64" fn() = std::mem::transmute(page.add(6));
                run();
                // A second thread that faulted at the same place meanwhile.
                assert!(patch(at + 6, [0x0f, 0x01, 0xfa], [0x0f, 0x1f, 0x00]));
                assert!(
                    !patch(at + 46, [0x0f, 0x01, 0xfa], [0x0f, 0x1f, 0x00]),
                    "crosses its sixteen bytes"
                );
            }
            assert_eq!(
                std::slice::from_raw_parts(page.add(46), 3),
                [0x0f, 0x01, 0xfa]
            );
            let mut m: MEMORY_BASIC_INFORMATION = std::mem::zeroed();
            VirtualQuery(page.cast(), &mut m, size_of_val(&m));
            assert_eq!(m.Protect, PAGE_EXECUTE_READ);
            VirtualFree(page.cast(), 0, MEM_RELEASE);
        }
    }

    /// `clzero` loops run as plain stores: the lines they name are zero and no other byte,
    /// and the pointer, the scratch register and the flags come out as from the loop.
    #[test]
    fn a_clzero_loop_becomes_plain_stores() {
        use windows_sys::Win32::System::Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_EXECUTE_READWRITE, VirtualAlloc, VirtualFree,
        };
        // Against rdx (rm rax, reg rdx) and against rcx, the scratch register.
        for (cmp, by) in [
            ([0x48, 0x39, 0xd0], 2),
            ([0x48, 0x39, 0xc8], 1),
            ([0x4c, 0x39, 0xc0], 8),
        ] {
            // SAFETY: memory of our own, written and run inside it.
            unsafe {
                let page = VirtualAlloc(
                    std::ptr::null(),
                    4096,
                    MEM_COMMIT | MEM_RESERVE,
                    PAGE_EXECUTE_READWRITE,
                ) as usize;
                assert_ne!(page, 0);
                // A function of (start, end): the start in rax, the end in its register, the
                // loop at 16, and what rax became.
                let mut head = vec![0x48, 0x89, 0xc8]; // mov rax, rcx
                match by {
                    1 => head.extend([0x48, 0x89, 0xd1]), // mov rcx, rdx
                    8 => head.extend([0x49, 0x89, 0xd0]), // mov r8, rdx
                    _ => {}
                }
                head.resize(16, 0x90);
                let mut code = head;
                code.extend([0x0f, 0x01, 0xfc, 0x48, 0x83, 0xc0, 0x40]);
                code.extend(cmp);
                code.extend([0x72, 0xf4]);
                code.push(0xc3);
                std::ptr::copy_nonoverlapping(code.as_ptr(), page as *mut u8, code.len());
                let mut context: CONTEXT = std::mem::zeroed();
                let mut block = vec![0xaau8; 64 * 8 + 64];
                let base = block.as_mut_ptr() as usize;
                let first = base.next_multiple_of(64);
                // The fault itself: emulated whole, and replaced.
                context.Rip = page as u64 + 16;
                context.Rax = first as u64;
                context.Rdx = first as u64 + 128;
                context.Rcx = context.Rdx;
                context.R8 = context.Rdx;
                assert_eq!(emulate(&mut context), CONTINUE_EXECUTION);
                assert_eq!(
                    (context.Rip, context.Rax),
                    (page as u64 + 28, first as u64 + 128)
                );
                if patches_clzero() {
                    assert_eq!(*((page + 16) as *const u8), 0xe9, "a jump");
                    block.fill(0xaa);
                    let run: extern "win64" fn(usize, usize) -> usize = std::mem::transmute(page);
                    // From inside a line to inside the third: whole lines, one past the end.
                    assert_eq!(run(first + 8, first + 130), first + 8 + 128);
                    let at = first - base;
                    assert!(block[at..at + 128].iter().all(|&b| b == 0));
                    assert!(
                        block[..at]
                            .iter()
                            .chain(&block[at + 128..])
                            .all(|&b| b == 0xaa)
                    );
                    // An end at the start: one line.
                    block.fill(0xaa);
                    assert_eq!(run(first, first), first + 64);
                    assert!(block[at..at + 64].iter().all(|&b| b == 0) && block[at + 64] == 0xaa);
                }
                VirtualFree(page as *mut _, 0, MEM_RELEASE);
            }
        }
        assert!(
            clzero_stub(0x1000, 0x2000, [0x48, 0x39, 0xc0]).is_none(),
            "against rax itself"
        );
        assert!(jump(0x1000, 0x9000_0000_0000).is_none(), "out of reach");
    }
}
