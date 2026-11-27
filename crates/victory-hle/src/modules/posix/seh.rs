//! Windows structured exception handling on macOS: what `ntdll` does on PC.
//!
//! An exception is dispatched by walking the stack with each image's unwind
//! table (`.pdata`) and calling the language handlers it names (the C++
//! runtime's `__CxxFrameHandler4`, `__C_specific_handler`). A handler that
//! catches calls `RtlUnwindEx`, which unwinds to the catching frame, runs
//! the catch block through the "consolidate" callback, and resumes there.
//!
//! Our own code has no `.pdata`, so two bridges carry unwind information of
//! their own (a small table we register, [`OUR_TABLE`]): the one handlers are
//! called through ([`victory_call_handler`]) unwinds straight to the frame of
//! the exception, and the one catch blocks return to
//! ([`victory_consolidate_return`]) unwinds to the catching frame. A walk from
//! inside a handler or a catch block thus never meets our frames.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use super::teb_field;
use crate::{HleExport, hle_export};

pub(super) fn exports() -> Vec<HleExport> {
    vec![
        hle_export!(RtlCaptureContext),
        hle_export!(RaiseException),
        hle_export!(RtlUnwindEx),
        hle_export!(RtlUnwind),
        hle_export!(RtlCaptureStackBackTrace),
        hle_export!(RtlLookupFunctionEntry),
        hle_export!(RtlVirtualUnwind),
        hle_export!(RtlPcToFileHeader),
        hle_export!(AddVectoredExceptionHandler),
        hle_export!(RemoveVectoredExceptionHandler),
    ]
}

/// The x64 `CONTEXT` (0x4d0 bytes).
#[repr(C, align(16))]
#[derive(Clone, Copy)]
pub(crate) struct Context {
    home: [u64; 6],
    flags: u32,
    mxcsr: u32,
    segments: [u16; 6],
    eflags: u32,
    debug: [u64; 6],
    /// rax, rcx, rdx, rbx, rsp, rbp, rsi, rdi, r8-r15: unwind codes' order.
    gpr: [u64; 16],
    rip: u64,
    float_header: [u8; 0xa0],
    xmm: [[u64; 2]; 16],
    rest: [u8; 0x4d0 - 0x2a0],
}

const RAX: usize = 0;
const RCX: usize = 1;
const RDX: usize = 2;
const RSP: usize = 4;
const R8: usize = 8;
const R9: usize = 9;

const _: () = assert!(size_of::<Context>() == 0x4d0);
const _: () = assert!(std::mem::offset_of!(Context, gpr) == 0x78);
const _: () = assert!(std::mem::offset_of!(Context, rip) == 0xf8);
const _: () = assert!(std::mem::offset_of!(Context, xmm) == 0x1a0);

/// `RUNTIME_FUNCTION`: a function's range and unwind information (RVAs).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeFunction {
    begin: u32,
    end: u32,
    unwind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct ExceptionRecord {
    code: u32,
    flags: u32,
    record: *mut ExceptionRecord,
    address: u64,
    count: u32,
    info: [u64; 15],
}

#[repr(C)]
struct DispatcherContext {
    control_pc: u64,
    image_base: u64,
    function_entry: *const RuntimeFunction,
    establisher_frame: u64,
    target_ip: u64,
    context: *mut Context,
    language_handler: usize,
    handler_data: *const c_void,
    history_table: *mut c_void,
    scope_index: u32,
    fill: u32,
}

const NONCONTINUABLE: u32 = 0x1;
const UNWINDING: u32 = 0x2;
const EXIT_UNWIND: u32 = 0x4;
const TARGET_UNWIND: u32 = 0x20;
const STATUS_UNWIND: u32 = 0xC000_0027;
const STATUS_UNWIND_CONSOLIDATE: u32 = 0x8000_0029;

const UNW_FLAG_EHANDLER: u32 = 1;
const UNW_FLAG_UHANDLER: u32 = 2;
const UNW_FLAG_CHAININFO: u8 = 4;

type Handler = unsafe extern "win64" fn(*mut ExceptionRecord, u64, *mut Context, *mut DispatcherContext) -> i32;
const CONTINUE_EXECUTION: i32 = 0;
const CONTINUE_SEARCH: i32 = 1;

/// Writes the registers of the code that called the stub into the context
/// at `{ctx}` (a register): as they were at the call, with `rsp` after the
/// return and `rip` the return address `{ret}` bytes up the stack.
macro_rules! save_caller {
    ($ctx:literal, $ret:literal) => {
        concat!(
            "mov [", $ctx, "+0x78], rax\n",
            "mov [", $ctx, "+0x80], rcx\n",
            "mov [", $ctx, "+0x88], rdx\n",
            "mov [", $ctx, "+0x90], rbx\n",
            "mov [", $ctx, "+0xa0], rbp\n",
            "mov [", $ctx, "+0xa8], rsi\n",
            "mov [", $ctx, "+0xb0], rdi\n",
            "mov [", $ctx, "+0xb8], r8\n",
            "mov [", $ctx, "+0xc0], r9\n",
            "mov [", $ctx, "+0xc8], r10\n",
            "mov [", $ctx, "+0xd0], r11\n",
            "mov [", $ctx, "+0xd8], r12\n",
            "mov [", $ctx, "+0xe0], r13\n",
            "mov [", $ctx, "+0xe8], r14\n",
            "mov [", $ctx, "+0xf0], r15\n",
            "lea rax, [rsp+", $ret, "+8]\n",
            "mov [", $ctx, "+0x98], rax\n",
            "mov rax, [rsp+", $ret, "]\n",
            "mov [", $ctx, "+0xf8], rax\n",
            "pushfq\n",
            "pop rax\n",
            "mov [", $ctx, "+0x44], eax\n",
            "stmxcsr [", $ctx, "+0x34]\n",
            "stmxcsr [", $ctx, "+0x118]\n",
            "movdqu [", $ctx, "+0x1a0], xmm0\n",
            "movdqu [", $ctx, "+0x1b0], xmm1\n",
            "movdqu [", $ctx, "+0x1c0], xmm2\n",
            "movdqu [", $ctx, "+0x1d0], xmm3\n",
            "movdqu [", $ctx, "+0x1e0], xmm4\n",
            "movdqu [", $ctx, "+0x1f0], xmm5\n",
            "movdqu [", $ctx, "+0x200], xmm6\n",
            "movdqu [", $ctx, "+0x210], xmm7\n",
            "movdqu [", $ctx, "+0x220], xmm8\n",
            "movdqu [", $ctx, "+0x230], xmm9\n",
            "movdqu [", $ctx, "+0x240], xmm10\n",
            "movdqu [", $ctx, "+0x250], xmm11\n",
            "movdqu [", $ctx, "+0x260], xmm12\n",
            "movdqu [", $ctx, "+0x270], xmm13\n",
            "movdqu [", $ctx, "+0x280], xmm14\n",
            "movdqu [", $ctx, "+0x290], xmm15\n",
            "mov word ptr [", $ctx, "+0x38], 0x33\n",
            "mov word ptr [", $ctx, "+0x42], 0x2b\n",
            "mov dword ptr [", $ctx, "+0x30], 0x10000f\n",
        )
    };
}

/// Loads the non-volatile registers (Windows' list) from the context at
/// `{ctx}`.
macro_rules! load_nonvolatile {
    ($ctx:literal) => {
        concat!(
            "mov rbx, [", $ctx, "+0x90]\n",
            "mov rbp, [", $ctx, "+0xa0]\n",
            "mov rsi, [", $ctx, "+0xa8]\n",
            "mov rdi, [", $ctx, "+0xb0]\n",
            "mov r12, [", $ctx, "+0xd8]\n",
            "mov r13, [", $ctx, "+0xe0]\n",
            "mov r14, [", $ctx, "+0xe8]\n",
            "mov r15, [", $ctx, "+0xf0]\n",
            "movdqu xmm6, [", $ctx, "+0x200]\n",
            "movdqu xmm7, [", $ctx, "+0x210]\n",
            "movdqu xmm8, [", $ctx, "+0x220]\n",
            "movdqu xmm9, [", $ctx, "+0x230]\n",
            "movdqu xmm10, [", $ctx, "+0x240]\n",
            "movdqu xmm11, [", $ctx, "+0x250]\n",
            "movdqu xmm12, [", $ctx, "+0x260]\n",
            "movdqu xmm13, [", $ctx, "+0x270]\n",
            "movdqu xmm14, [", $ctx, "+0x280]\n",
            "movdqu xmm15, [", $ctx, "+0x290]\n",
        )
    };
}

/// A stub named `_{name}` (macOS prefixes C symbols) that hands its
/// caller's context to `{impl}`, which never returns: 0x508 bytes of
/// stack, the context 16-aligned at +0x30.
macro_rules! with_caller_context {
    ($name:literal, $impl:path) => {
        std::arch::global_asm!(
            ".p2align 4",
            concat!(".globl _", $name),
            concat!("_", $name, ":"),
            "sub rsp, 0x508",
            "mov [rsp+0x28], r11",
            "lea r11, [rsp+0x30]",
            save_caller!("r11", "0x508"),
            "mov rax, [rsp+0x28]",
            "mov [r11+0xd0], rax",
            "mov rcx, r11",
            "call {imp}",
            "ud2",
            imp = sym $impl,
        );
    };
}

with_caller_context!("RaiseException", raise);
with_caller_context!("RtlUnwindEx", unwind_ex);
with_caller_context!("RtlUnwind", unwind_legacy);
with_caller_context!("RtlCaptureStackBackTrace", back_trace);

std::arch::global_asm!(
    // RtlCaptureContext(rcx: context): the caller's registers.
    ".p2align 4",
    ".globl _RtlCaptureContext",
    "_RtlCaptureContext:",
    save_caller!("rcx", "0"),
    "mov rax, [rcx+0x78]",
    "ret",
    // victory_restore_context(rcx: context): never returns.
    ".p2align 4",
    ".globl _victory_restore_context",
    "_victory_restore_context:",
    "mov rax, [rcx+0x98]",
    "mov rdx, [rcx+0xf8]",
    "mov [rax-8], rdx",
    "ldmxcsr [rcx+0x34]",
    "movdqu xmm0, [rcx+0x1a0]",
    "movdqu xmm1, [rcx+0x1b0]",
    "movdqu xmm2, [rcx+0x1c0]",
    "movdqu xmm3, [rcx+0x1d0]",
    "movdqu xmm4, [rcx+0x1e0]",
    "movdqu xmm5, [rcx+0x1f0]",
    load_nonvolatile!("rcx"),
    "mov rdx, [rcx+0x88]",
    "mov r8, [rcx+0xb8]",
    "mov r9, [rcx+0xc0]",
    "mov r10, [rcx+0xc8]",
    "mov r11, [rcx+0xd0]",
    "push qword ptr [rcx+0x44]",
    "popfq",
    "lea rsp, [rax-8]",
    "mov rax, [rcx+0x78]",
    "mov rcx, [rcx+0x80]",
    "ret",
    // victory_call_handler(rcx: &HandlerCall) -> eax. The handler runs
    // with the non-volatile registers of `call.frame_context`, above a
    // machine frame holding its rip and rsp, so a walk from inside the
    // handler continues at that frame (our table: alloc 32, machine frame).
    ".p2align 4",
    ".globl _victory_call_handler",
    "_victory_call_handler:",
    "push rbx",
    "push rbp",
    "push rsi",
    "push rdi",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "sub rsp, 0xa8",
    "movdqu [rsp+0x00], xmm6",
    "movdqu [rsp+0x10], xmm7",
    "movdqu [rsp+0x20], xmm8",
    "movdqu [rsp+0x30], xmm9",
    "movdqu [rsp+0x40], xmm10",
    "movdqu [rsp+0x50], xmm11",
    "movdqu [rsp+0x60], xmm12",
    "movdqu [rsp+0x70], xmm13",
    "movdqu [rsp+0x80], xmm14",
    "movdqu [rsp+0x90], xmm15",
    "mov r10, rcx",
    "mov r11, [r10+40]",
    "sub rsp, 0x30",
    "mov rax, [r11+0xf8]",
    "mov [rsp], rax",
    "mov qword ptr [rsp+8], 0x33",
    "mov qword ptr [rsp+16], 0x202",
    "mov rax, [r11+0x98]",
    "mov [rsp+24], rax",
    "mov qword ptr [rsp+32], 0x2b",
    "sub rsp, 0x20",
    load_nonvolatile!("r11"),
    "mov rax, [r10]",
    "mov rcx, [r10+8]",
    "mov rdx, [r10+16]",
    "mov r8, [r10+24]",
    "mov r9, [r10+32]",
    "call rax",
    ".globl _victory_call_handler_return",
    "_victory_call_handler_return:",
    "nop",
    "add rsp, 0x50",
    "movdqu xmm6, [rsp+0x00]",
    "movdqu xmm7, [rsp+0x10]",
    "movdqu xmm8, [rsp+0x20]",
    "movdqu xmm9, [rsp+0x30]",
    "movdqu xmm10, [rsp+0x40]",
    "movdqu xmm11, [rsp+0x50]",
    "movdqu xmm12, [rsp+0x60]",
    "movdqu xmm13, [rsp+0x70]",
    "movdqu xmm14, [rsp+0x80]",
    "movdqu xmm15, [rsp+0x90]",
    "add rsp, 0xa8",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rdi",
    "pop rsi",
    "pop rbp",
    "pop rbx",
    "ret",
    ".globl _victory_call_handler_end",
    "_victory_call_handler_end:",
    // victory_consolidate(rcx: area, rdx: callback, r8: record): never
    // returns. `area` is laid out by `consolidate`: the return address at
    // +0, home space, a machine frame at +40 (the catching frame's rip and
    // rsp), the context's address at +80. The callback runs with the
    // catching frame's non-volatile registers and returns the address to
    // resume at, in rax.
    ".p2align 4",
    ".globl _victory_consolidate",
    "_victory_consolidate:",
    "mov r11, [rcx+80]",
    load_nonvolatile!("r11"),
    "mov rsp, rcx",
    "mov rax, rdx",
    "mov rcx, r8",
    "jmp rax",
    ".p2align 4",
    ".globl _victory_consolidate_return",
    "_victory_consolidate_return:",
    "mov rcx, [rsp+72]",
    "mov rdx, rax",
    "call {done}",
    "ud2",
    ".globl _victory_consolidate_end",
    "_victory_consolidate_end:",
    done = sym consolidate_done,
);

unsafe extern "win64" {
    fn RtlCaptureContext(context: *mut Context);
    fn RaiseException(code: u32, flags: u32, count: u32, args: *const u64);
    fn RtlUnwindEx(frame: u64, target: u64, record: *mut ExceptionRecord, value: u64, context: *mut Context, history: *mut c_void);
    fn RtlUnwind(frame: u64, target: u64, record: *mut ExceptionRecord, value: u64);
    fn RtlCaptureStackBackTrace(skip: u32, count: u32, frames: *mut u64, hash: *mut u32) -> u16;
    fn victory_restore_context(context: *const Context) -> !;
    fn victory_call_handler(call: *const HandlerCall) -> i32;
    fn victory_consolidate(area: usize, callback: usize, record: *mut ExceptionRecord) -> !;
    static victory_call_handler_return: u8;
    static victory_call_handler_end: u8;
    static victory_consolidate_return: u8;
    static victory_consolidate_end: u8;
}

#[repr(C)]
struct HandlerCall {
    handler: Handler,
    record: *mut ExceptionRecord,
    frame: u64,
    context: *mut Context,
    dispatch: *mut DispatcherContext,
    frame_context: *const Context,
}

/// Our bridges' unwind information: "sub rsp, 32" after a machine frame.
#[repr(C, align(4))]
struct UnwindInfo([u8; 8]);
static BRIDGE_UNWIND: UnwindInfo = UnwindInfo([0x01, 0x00, 0x02, 0x00, 0x00, 0x32, 0x00, 0x0a]);

/// Our bridges' table: a base below all of them, and their entries.
struct OurTable {
    base: usize,
    functions: [RuntimeFunction; 2],
}

static OUR_TABLE: OnceLock<OurTable> = OnceLock::new();

fn our_table() -> &'static OurTable {
    OUR_TABLE.get_or_init(|| {
        // SAFETY: addresses of our own symbols.
        let (a0, a1, b0, b1, info) = unsafe {
            (
                &raw const victory_call_handler_return as usize,
                &raw const victory_call_handler_end as usize,
                &raw const victory_consolidate_return as usize,
                &raw const victory_consolidate_end as usize,
                &raw const BRIDGE_UNWIND as usize,
            )
        };
        let base = a0.min(b0).min(info);
        let rva = |a: usize| u32::try_from(a - base).expect("our code and data lie within 4 GB");
        OurTable {
            base,
            functions: [
                RuntimeFunction { begin: rva(a0), end: rva(a1), unwind: rva(info) },
                RuntimeFunction { begin: rva(b0), end: rva(b1), unwind: rva(info) },
            ],
        }
    })
}

/// The unwind table (`.pdata`) of the image at `base`, from its headers.
fn image_table(base: usize) -> &'static [RuntimeFunction] {
    // SAFETY: a mapped PE image's headers: e_lfanew, then the optional
    // header's data directories (exception = 3).
    unsafe {
        let nt = base + (base as *const u8).add(0x3c).cast::<u32>().read_unaligned() as usize;
        let directory = (nt + 0x18 + 112 + 3 * 8) as *const u32;
        let (rva, size) = (directory.read_unaligned(), directory.add(1).read_unaligned());
        if rva == 0 {
            return &[];
        }
        std::slice::from_raw_parts((base + rva as usize) as *const RuntimeFunction, size as usize / 12)
    }
}

/// The function entry for `pc` and its image's base.
fn lookup(pc: u64) -> Option<(u64, *const RuntimeFunction)> {
    let pc = pc as usize;
    let (base, table) = match victory_loader::image_containing(pc) {
        Some((base, _)) => (base, image_table(base)),
        None => {
            let ours = our_table();
            (ours.base, &ours.functions[..])
        }
    };
    let rva = u32::try_from(pc.checked_sub(base)?).ok()?;
    let i = table.partition_point(|f| f.end <= rva);
    let f = table.get(i).filter(|f| f.begin <= rva)?;
    Some((base as u64, f as *const RuntimeFunction))
}

extern "win64" fn RtlLookupFunctionEntry(pc: u64, base: *mut u64, _history: *mut c_void) -> *const RuntimeFunction {
    match lookup(pc) {
        Some((b, f)) => {
            // SAFETY: the caller's out pointer.
            unsafe { base.write(b) };
            f
        }
        None => std::ptr::null(),
    }
}

extern "win64" fn RtlPcToFileHeader(pc: usize, base: *mut usize) -> usize {
    let found = victory_loader::image_containing(pc).map_or(0, |(b, _)| b);
    // SAFETY: the caller's out pointer.
    unsafe { base.write(found) };
    found
}

/// Unwind operations.
const PUSH_NONVOL: u8 = 0;
const ALLOC_LARGE: u8 = 1;
const ALLOC_SMALL: u8 = 2;
const SET_FPREG: u8 = 3;
const SAVE_NONVOL: u8 = 4;
const SAVE_NONVOL_FAR: u8 = 5;
const EPILOG: u8 = 6;
const SAVE_XMM128: u8 = 8;
const SAVE_XMM128_FAR: u8 = 9;
const PUSH_MACHFRAME: u8 = 10;

fn read_u64(at: u64) -> u64 {
    // SAFETY: stack slots named by unwind information.
    unsafe { (at as *const u64).read_unaligned() }
}

/// Undoes the prologue of the function `f` (in the image at `base`) at
/// `context`, which becomes its caller's. The language handler of
/// `kind` (`UNW_FLAG_EHANDLER`/`UHANDLER`) if the function has one and `pc`
/// is past its prologue, with its data and the establisher frame.
fn virtual_unwind(kind: u32, base: u64, f: *const RuntimeFunction, context: &mut Context) -> (Option<(Handler, *const c_void)>, u64) {
    let pc = context.rip;
    let mut f = f;
    let mut machine_frame = false;
    let mut frame;
    let mut prolog_offset;
    let mut info;
    loop {
        // SAFETY: the entry and its unwind information lie in the image.
        let func = unsafe { *f };
        info = (base + u64::from(func.unwind)) as *const u8;
        // SAFETY: UNWIND_INFO's header.
        let (flags, prolog, count, frame_byte) = unsafe { (*info >> 3, *info.add(1), *info.add(2), *info.add(3)) };
        let codes = unsafe { info.add(4) } as *const [u8; 2];
        prolog_offset = pc.wrapping_sub(base + u64::from(func.begin));
        let frame_reg = usize::from(frame_byte & 0xf);
        frame = if frame_reg != 0 {
            context.gpr[frame_reg].wrapping_sub(u64::from(frame_byte >> 4) * 16)
        } else {
            context.gpr[RSP]
        };
        if prolog_offset > u64::from(prolog) && flags & UNW_FLAG_CHAININFO == 0 && in_epilog(pc, base, &func) {
            run_epilog(pc, context);
            return (None, frame);
        }
        let slot = |i: usize| -> [u8; 2] {
            // SAFETY: inside the code array of `count` slots.
            unsafe { codes.add(i).read() }
        };
        let u16_at = |i: usize| u64::from(u16::from_le_bytes(slot(i)));
        let u32_at = |i: usize| u16_at(i) | u16_at(i + 1) << 16;
        let mut i = 0;
        while i < usize::from(count) {
            let [offset, op_info] = slot(i);
            let (op, op_info) = (op_info & 0xf, op_info >> 4);
            let size = match op {
                ALLOC_LARGE if op_info != 0 => 3,
                ALLOC_LARGE | SAVE_NONVOL | SAVE_XMM128 | EPILOG => 2,
                SAVE_NONVOL_FAR | SAVE_XMM128_FAR => 3,
                _ => 1,
            };
            if prolog_offset >= u64::from(offset) || op == EPILOG {
                let reg = usize::from(op_info);
                match op {
                    PUSH_NONVOL => {
                        context.gpr[reg] = read_u64(context.gpr[RSP]);
                        context.gpr[RSP] += 8;
                    }
                    ALLOC_LARGE => context.gpr[RSP] += if op_info != 0 { u32_at(i + 1) } else { u16_at(i + 1) * 8 },
                    ALLOC_SMALL => context.gpr[RSP] += (u64::from(op_info) + 1) * 8,
                    SET_FPREG => context.gpr[RSP] = frame,
                    SAVE_NONVOL => context.gpr[reg] = read_u64(frame + u16_at(i + 1) * 8),
                    SAVE_NONVOL_FAR => context.gpr[reg] = read_u64(frame + u32_at(i + 1)),
                    SAVE_XMM128 => {
                        let at = frame + u16_at(i + 1) * 16;
                        context.xmm[reg] = [read_u64(at), read_u64(at + 8)];
                    }
                    SAVE_XMM128_FAR => {
                        let at = frame + u32_at(i + 1);
                        context.xmm[reg] = [read_u64(at), read_u64(at + 8)];
                    }
                    PUSH_MACHFRAME => {
                        if op_info != 0 {
                            context.gpr[RSP] += 8;
                        }
                        context.rip = read_u64(context.gpr[RSP]);
                        context.gpr[RSP] = read_u64(context.gpr[RSP] + 24);
                        machine_frame = true;
                    }
                    _ => {}
                }
            }
            i += size;
        }
        if flags & UNW_FLAG_CHAININFO == 0 {
            break;
        }
        // SAFETY: the chained entry follows the (even-padded) codes.
        f = unsafe { codes.add((usize::from(count) + 1) & !1) } as *const RuntimeFunction;
    }
    if !machine_frame {
        context.rip = read_u64(context.gpr[RSP]);
        context.gpr[RSP] += 8;
    }
    // SAFETY: the header and the handler's RVA after the codes.
    let (flags, prolog, count) = unsafe { (u32::from(*info >> 3), *info.add(1), *info.add(2)) };
    if flags & kind == 0 || prolog_offset < u64::from(prolog) {
        return (None, frame);
    }
    // SAFETY: as above.
    let at = unsafe { info.add(4 + 2 * ((usize::from(count) + 1) & !1)) };
    let rva = unsafe { at.cast::<u32>().read_unaligned() };
    // SAFETY: the image's own handler, a function of the title.
    let handler: Handler = unsafe { std::mem::transmute(base + u64::from(rva)) };
    (Some((handler, unsafe { at.add(4) }.cast())), frame)
}

/// Whether `pc` is in an epilogue: `add rsp`/`lea rsp` then pops, ending
/// in `ret` or a jump out of the function (Windows' rules for x64).
fn in_epilog(pc: u64, base: u64, f: &RuntimeFunction) -> bool {
    let at = |i: u64| -> u8 {
        // SAFETY: code bytes of the image.
        unsafe { *((pc + i) as *const u8) }
    };
    let mut i = 0;
    if at(0) & 0xf8 == 0x48 {
        match at(1) {
            0x81 if at(0) == 0x48 && at(2) == 0xc4 => i = 7,
            0x83 if at(0) == 0x48 && at(2) == 0xc4 => i = 4,
            0x8d if at(0) & 6 == 0 && (at(2) >> 3) & 7 == 4 && at(2) & 7 != 4 => match at(2) >> 6 {
                1 => i = 4,
                2 => i = 7,
                _ => return false,
            },
            0x81 | 0x83 | 0x8d => return false,
            _ => {}
        }
    }
    loop {
        if at(i) & 0xf0 == 0x40 {
            i += 1;
        }
        match at(i) {
            0x58..=0x5f => i += 1,
            0xc2 | 0xc3 => return true,
            0xf3 => return at(i + 1) == 0xc3,
            0xe9 | 0xeb => {
                let target = if at(i) == 0xe9 {
                    // SAFETY: the jump's operand.
                    let rel = unsafe { ((pc + i + 1) as *const i32).read_unaligned() };
                    (pc + i + 5).wrapping_add_signed(i64::from(rel))
                } else {
                    (pc + i + 2).wrapping_add_signed(i64::from(at(i + 1) as i8))
                };
                let range = base + u64::from(f.begin)..base + u64::from(f.end);
                return !range.contains(&target);
            }
            0xff => return at(i + 1) == 0x25 || (at(i + 1) >> 3) & 7 == 4,
            _ => return false,
        }
    }
}

/// Runs the rest of an epilogue on `context`.
fn run_epilog(pc: u64, context: &mut Context) {
    let at = |i: u64| -> u8 {
        // SAFETY: code bytes, checked by `in_epilog`.
        unsafe { *((pc + i) as *const u8) }
    };
    let mut i = 0;
    if at(0) & 0xf8 == 0x48 {
        let imm32 = |i: u64| {
            // SAFETY: as above.
            i64::from(unsafe { ((pc + i) as *const i32).read_unaligned() })
        };
        match at(1) {
            0x81 => {
                context.gpr[RSP] = context.gpr[RSP].wrapping_add_signed(imm32(3));
                i = 7;
            }
            0x83 => {
                context.gpr[RSP] = context.gpr[RSP].wrapping_add_signed(i64::from(at(3) as i8));
                i = 4;
            }
            0x8d => {
                let reg = usize::from(at(2) & 7) + if at(0) & 1 != 0 { 8 } else { 0 };
                let (disp, len) = if at(2) >> 6 == 1 { (i64::from(at(3) as i8), 4) } else { (imm32(3), 7) };
                context.gpr[RSP] = context.gpr[reg].wrapping_add_signed(disp);
                i = len;
            }
            _ => {}
        }
    }
    loop {
        let rex_b = if at(i) & 0xf0 == 0x40 {
            let b = at(i) & 1;
            i += 1;
            b
        } else {
            0
        };
        match at(i) {
            r @ 0x58..=0x5f => {
                context.gpr[usize::from(r - 0x58) + 8 * usize::from(rex_b)] = read_u64(context.gpr[RSP]);
                context.gpr[RSP] += 8;
                i += 1;
            }
            _ => {
                // `ret`, or a jump out of the function: as a return.
                context.rip = read_u64(context.gpr[RSP]);
                context.gpr[RSP] += 8;
                return;
            }
        }
    }
}

extern "win64" fn RtlVirtualUnwind(
    kind: u32,
    base: u64,
    _pc: u64,
    f: *const RuntimeFunction,
    context: *mut Context,
    data: *mut *const c_void,
    frame: *mut u64,
    _pointers: *mut c_void,
) -> Option<Handler> {
    // SAFETY: the caller's context and out pointers.
    let (handler, establisher) = virtual_unwind(kind, base, f, unsafe { &mut *context });
    unsafe {
        frame.write(establisher);
        data.write(handler.map_or(std::ptr::null(), |h| h.1));
    }
    handler.map(|h| h.0)
}

/// Steps `context` to its caller's: by the unwind table, or as a leaf
/// function (return address at rsp). The handler of `kind` and the frame.
fn step(kind: u32, context: &mut Context) -> Option<(Option<(Handler, *const c_void)>, u64, u64, *const RuntimeFunction)> {
    if context.rip == 0 {
        return None;
    }
    match lookup(context.rip) {
        Some((base, f)) => {
            let (handler, frame) = virtual_unwind(kind, base, f, context);
            Some((handler, frame, base, f))
        }
        None => {
            let frame = context.gpr[RSP];
            context.rip = read_u64(frame);
            context.gpr[RSP] += 8;
            Some((None, frame, 0, std::ptr::null()))
        }
    }
}

/// Whether `rsp` lies in the calling thread's stack.
fn on_stack(rsp: u64) -> bool {
    let (base, limit): (u64, u64) = (teb_field(0x08), teb_field(0x10));
    base == 0 || (limit..base).contains(&rsp)
}

fn call_handler(handler: Handler, record: *mut ExceptionRecord, frame: u64, context: *mut Context, dispatch: &mut DispatcherContext, frame_context: *const Context) -> i32 {
    let call = HandlerCall {
        handler,
        record,
        frame,
        context,
        dispatch,
        frame_context,
    };
    // SAFETY: the title's handler, called through our bridge.
    unsafe { victory_call_handler(&call) }
}

/// Vectored handlers, first to last.
static VECTORED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

extern "win64" fn AddVectoredExceptionHandler(first: u32, handler: usize) -> usize {
    let mut list = VECTORED.lock().unwrap_or_else(|e| e.into_inner());
    if first != 0 {
        list.insert(0, handler);
    } else {
        list.push(handler);
    }
    handler
}

extern "win64" fn RemoveVectoredExceptionHandler(handle: usize) -> u32 {
    let mut list = VECTORED.lock().unwrap_or_else(|e| e.into_inner());
    let before = list.len();
    list.retain(|&h| h != handle);
    u32::from(list.len() != before)
}

/// Finds a handler for `record` raised at `context`: true if one asked to
/// continue execution (at `context`, which it may have changed).
pub(crate) fn dispatch(record: &mut ExceptionRecord, context: &mut Context) -> bool {
    let vectored = VECTORED.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for handler in vectored {
        let mut pointers = [record as *mut ExceptionRecord as usize, context as *mut Context as usize];
        // SAFETY: a vectored handler the title registered.
        let f: unsafe extern "win64" fn(*mut [usize; 2]) -> i32 = unsafe { std::mem::transmute(handler) };
        if unsafe { f(&mut pointers) } == -1 {
            return true;
        }
    }
    let mut frame_context = *context;
    let frame_at: *mut Context = &mut frame_context;
    while on_stack(frame_context.gpr[RSP]) {
        let mut caller = frame_context;
        let Some((handler, frame, base, entry)) = step(UNW_FLAG_EHANDLER, &mut caller) else { break };
        if let Some((handler, data)) = handler {
            let mut dc = DispatcherContext {
                control_pc: frame_context.rip,
                image_base: base,
                function_entry: entry,
                establisher_frame: frame,
                target_ip: 0,
                context: frame_at,
                language_handler: handler as usize,
                handler_data: data,
                history_table: std::ptr::null_mut(),
                scope_index: 0,
                fill: 0,
            };
            match call_handler(handler, record, frame, context, &mut dc, frame_at) {
                CONTINUE_EXECUTION if record.flags & NONCONTINUABLE == 0 => return true,
                CONTINUE_SEARCH => {}
                other => tracing::warn!(other, code = format_args!("{:#x}", record.code), "exception handler answer not handled"),
            }
        }
        frame_context = caller;
    }
    false
}

/// `module+offset` for an address in a title image, else the address.
fn describe(address: u64) -> String {
    match victory_loader::image_containing(address as usize) {
        Some((base, _)) => {
            let name = crate::runtime::image_path(base)
                .and_then(|p| Some(p.file_name()?.to_string_lossy().into_owned()))
                .unwrap_or_else(|| format!("{base:#x}"));
            format!("{name}+{:#x}", address as usize - base)
        }
        None => format!("{address:#x}"),
    }
}

/// Every other thread's title frames, the threads stopped first (we are
/// crashing): `thread n: frames`, for threads in title code only.
fn other_threads() -> Vec<String> {
    unsafe extern "C" {
        static mach_task_self_: u32;
        fn mach_thread_self() -> u32;
        fn task_threads(task: u32, threads: *mut *mut u32, count: *mut u32) -> i32;
        fn thread_suspend(thread: u32) -> i32;
        fn thread_get_state(thread: u32, flavor: i32, state: *mut u64, count: *mut u32) -> i32;
    }
    const X86_THREAD_STATE64: i32 = 4;
    let mut out = Vec::new();
    let (mut threads, mut count) = (std::ptr::null_mut(), 0u32);
    // SAFETY: Mach queries about our own task; the list is the kernel's.
    unsafe {
        if task_threads(mach_task_self_, &mut threads, &mut count) != 0 {
            return out;
        }
        let me = mach_thread_self();
        for i in 0..count as usize {
            let thread = *threads.add(i);
            if thread == me || thread_suspend(thread) != 0 {
                continue;
            }
            let mut state = [0u64; 21];
            let mut words = 42u32;
            if thread_get_state(thread, X86_THREAD_STATE64, state.as_mut_ptr(), &mut words) != 0 {
                continue;
            }
            let mut context: Context = std::mem::zeroed();
            for (windows, mac) in FROM_MAC.iter().enumerate() {
                context.gpr[windows] = state[*mac];
            }
            context.rip = state[16];
            // Stopped in our own code (a wait, mostly): from the first
            // return address into title code on its stack.
            if victory_loader::image_containing(context.rip as usize).is_none() {
                let rsp = context.gpr[RSP];
                let found = (0..2048u64).map(|i| rsp + 8 * i).take_while(|&at| crate::modules::xmem::readable(at as usize, 8)).find(|&at| {
                    let v = (at as *const u64).read();
                    victory_loader::image_containing(v as usize).is_some()
                        && crate::modules::xmem::readable(v as usize - 6, 6)
                        && (*((v - 5) as *const u8) == 0xe8 || *((v - 6) as *const u8) == 0xff || *((v - 2) as *const u8) == 0xff || *((v - 3) as *const u8) == 0xff)
                });
                let Some(at) = found else { continue };
                context.rip = (at as *const u64).read();
                context.gpr[RSP] = at + 8;
            }
            let walk = frames_where(&context, |rsp| crate::modules::xmem::readable(rsp as usize, 8));
            if walk.iter().any(|f| f.contains('+')) {
                out.push(format!("thread {i}: {}", walk.join(" < ")));
            }
        }
    }
    out
}

/// The frames from `context` up, as `module+offset`, at most 24.
fn frames(context: &Context) -> Vec<String> {
    frames_where(context, on_stack)
}

/// As [`frames`], walking while `fine(rsp)`.
fn frames_where(context: &Context, fine: impl Fn(u64) -> bool) -> Vec<String> {
    let mut walk = *context;
    let mut out = Vec::new();
    while out.len() < 24 && fine(walk.gpr[RSP]) && walk.rip != 0 {
        out.push(describe(walk.rip));
        if step(0, &mut walk).is_none() {
            break;
        }
    }
    out
}

/// The registers, and the stack's top 32 slots.
fn registers(context: &Context) -> String {
    const NAMES: [&str; 16] = ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
    let regs: Vec<String> = NAMES.iter().zip(context.gpr).map(|(n, v)| format!("{n}={v:#x}")).collect();
    let rsp = context.gpr[RSP];
    let slots: Vec<String> = (0..32)
        .map(|i| rsp + i * 8)
        .take_while(|&at| on_stack(at))
        // SAFETY: slots of the thread's own stack.
        .map(|at| format!("{:#x}", unsafe { (at as *const u64).read() }))
        .collect();
    let memory: Vec<String> = NAMES
        .iter()
        .zip(context.gpr)
        .filter(|&(_, v)| v > 0x10000 && crate::modules::xmem::readable(v as usize, 48))
        .map(|(n, v)| {
            // SAFETY: readable, checked just above.
            let bytes = unsafe { std::slice::from_raw_parts(v as *const u64, 6) };
            format!("[{n}] {:x?}", bytes)
        })
        .collect();
    format!(
        "{}\n  stack slots from rsp: {}\n  memory: {}",
        regs.join(" "),
        slots.join(" "),
        memory.join(" ")
    )
}

fn unhandled(record: &ExceptionRecord, context: &Context) -> ! {
    let text = format!(
        "unhandled exception {:#x} at {} ({} parameters: {:x?})\n  frames: {}\n  {}\n  other threads:\n    {}",
        record.code,
        describe(record.address),
        record.count,
        &record.info[..record.count.min(15) as usize],
        frames(context).join(" < "),
        registers(context),
        other_threads().join("\n    ")
    );
    tracing::error!("{text}");
    victory_loader::log_crash(&text);
    std::process::abort()
}

/// `RaiseException`, with its caller's context (its arguments in it).
extern "win64" fn raise(context: &mut Context) -> ! {
    let count = (context.gpr[R8] as usize).min(15);
    let mut record = ExceptionRecord {
        code: context.gpr[RCX] as u32,
        flags: context.gpr[RDX] as u32 & NONCONTINUABLE,
        record: std::ptr::null_mut(),
        address: context.rip,
        count: count as u32,
        info: [0; 15],
    };
    let args = context.gpr[R9] as *const u64;
    if !args.is_null() {
        // SAFETY: the caller's `count` arguments.
        record.info[..count].copy_from_slice(unsafe { std::slice::from_raw_parts(args, count) });
    }
    if dispatch(&mut record, context) {
        // SAFETY: resumes after the call to RaiseException.
        unsafe { victory_restore_context(context) }
    }
    unhandled(&record, context)
}

/// `RtlUnwindEx(frame, target, record, value, context, history)`, with its
/// caller's context (arguments in rcx, rdx, r8, r9, then on the stack).
extern "win64" fn unwind_ex(context: &mut Context) -> ! {
    let record = context.gpr[R8] as *mut ExceptionRecord;
    unwind(context.gpr[RCX], context.gpr[RDX], record, context.gpr[R9], context)
}

/// `RtlUnwind(frame, target, record, value)`.
extern "win64" fn unwind_legacy(context: &mut Context) -> ! {
    let record = context.gpr[R8] as *mut ExceptionRecord;
    unwind(context.gpr[RCX], context.gpr[RDX], record, context.gpr[R9], context)
}

/// Unwinds from `start` (the caller of `RtlUnwindEx`) to the frame
/// `target_frame`, calling each frame's unwind handler, then resumes there
/// at `target_ip` with `value` in rax.
fn unwind(target_frame: u64, target_ip: u64, record: *mut ExceptionRecord, value: u64, start: &Context) -> ! {
    let mut local = ExceptionRecord {
        code: STATUS_UNWIND,
        flags: 0,
        record: std::ptr::null_mut(),
        address: start.rip,
        count: 0,
        info: [0; 15],
    };
    // SAFETY: the caller's record, or ours.
    let record = unsafe { &mut *(if record.is_null() { &mut local } else { record }) };
    record.flags |= UNWINDING;
    if target_frame == 0 {
        record.flags |= EXIT_UNWIND;
    }
    let mut frame_context = *start;
    let frame_at: *mut Context = &mut frame_context;
    loop {
        if !on_stack(frame_context.gpr[RSP]) {
            tracing::error!(target = format_args!("{target_frame:#x}"), "unwind ran off the stack");
            unhandled(record, start)
        }
        let mut caller = frame_context;
        let Some((handler, frame, base, entry)) = step(UNW_FLAG_UHANDLER, &mut caller) else { unhandled(record, start) };
        if target_frame != 0 && frame > target_frame {
            tracing::error!(frame = format_args!("{frame:#x}"), target = format_args!("{target_frame:#x}"), "unwind passed its target frame");
            unhandled(record, start)
        }
        if frame == target_frame {
            record.flags |= TARGET_UNWIND;
        }
        if let Some((handler, data)) = handler {
            let mut dc = DispatcherContext {
                control_pc: frame_context.rip,
                image_base: base,
                function_entry: entry,
                establisher_frame: frame,
                target_ip,
                context: frame_at,
                language_handler: handler as usize,
                handler_data: data,
                history_table: std::ptr::null_mut(),
                scope_index: 0,
                fill: 0,
            };
            let answer = call_handler(handler, record, frame, frame_at, &mut dc, frame_at);
            if answer != CONTINUE_SEARCH {
                tracing::warn!(answer, "unwind handler answer not handled");
            }
        }
        if frame == target_frame {
            break;
        }
        frame_context = caller;
    }
    frame_context.gpr[RAX] = value;
    frame_context.rip = target_ip;
    if record.code == STATUS_UNWIND_CONSOLIDATE && record.count >= 1 {
        consolidate(&frame_context, record)
    }
    // SAFETY: the target frame's registers.
    unsafe { victory_restore_context(&frame_context) }
}

/// Runs the consolidate callback (`record.info[0]`, the catch block's
/// caller) below the target frame, then resumes the frame where it says.
fn consolidate(target: &Context, record: &mut ExceptionRecord) -> ! {
    // Below the target frame: everything there is dead, and we run far
    // deeper (two 0x508-byte stubs and the handlers lie between).
    let area_top = (target.gpr[RSP] - 0x100) & !15;
    let context_at = area_top - size_of::<Context>() as u64;
    let area = (context_at - 96) | 8;
    // SAFETY: the dead stack below the target frame, laid out as
    // `victory_consolidate` expects.
    unsafe {
        (context_at as *mut Context).write(*target);
        let slots = area as *mut u64;
        slots.write(&raw const victory_consolidate_return as u64);
        slots.add(5).write(target.rip);
        slots.add(6).write(0x33);
        slots.add(7).write(0x202);
        slots.add(8).write(target.gpr[RSP]);
        slots.add(9).write(0x2b);
        slots.add(10).write(context_at);
        victory_consolidate(area as usize, record.info[0] as usize, record)
    }
}

/// Where the catch block's caller said to resume.
extern "win64" fn consolidate_done(context: *mut Context, resume: u64) -> ! {
    // SAFETY: the copy `consolidate` made, still below the target frame.
    unsafe {
        (*context).rip = resume;
        victory_restore_context(context)
    }
}

/// `RtlCaptureStackBackTrace(skip, count, frames, hash)`.
extern "win64" fn back_trace(context: &mut Context) -> ! {
    let (skip, count) = (context.gpr[RCX] as usize, context.gpr[RDX] as usize);
    let (frames, hash) = (context.gpr[R8] as *mut u64, context.gpr[R9] as *mut u32);
    let mut walk = *context;
    let mut taken = 0usize;
    let mut sum = 0u32;
    let mut index = 0;
    while taken < count && on_stack(walk.gpr[RSP]) && walk.rip != 0 {
        if index >= skip {
            // SAFETY: the caller's array of `count` slots.
            unsafe { frames.add(taken).write(walk.rip) };
            sum = sum.wrapping_add(walk.rip as u32);
            taken += 1;
        }
        index += 1;
        if step(0, &mut walk).is_none() {
            break;
        }
    }
    if !hash.is_null() {
        // SAFETY: the caller's out pointer.
        unsafe { hash.write(sum) };
    }
    context.gpr[RAX] = taken as u64;
    // SAFETY: returns to the caller with the count in rax.
    unsafe { victory_restore_context(context) }
}

/// The thread state macOS gives a signal handler (x86-64): the exception
/// state, then rax, rbx, rcx, rdx, rdi, rsi, rbp, rsp, r8-r15, rip, rflags,
/// cs, fs, gs, then the float state (MXCSR at 32, xmm0 at 168).
#[repr(C)]
struct MachineContext {
    trapno: u16,
    cpu: u16,
    err: u32,
    fault: u64,
    ss: [u64; 21],
    fs: [u8; 524],
}

/// Windows' register order from macOS's.
const FROM_MAC: [usize; 16] = [0, 2, 3, 1, 7, 6, 5, 4, 8, 9, 10, 11, 12, 13, 14, 15];

/// What a signal becomes, put on the faulting thread's stack.
#[repr(C, align(16))]
struct Fault {
    context: Context,
    record: ExceptionRecord,
}

const ACCESS_VIOLATION: u32 = 0xC000_0005;
const ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
const BREAKPOINT: u32 = 0x8000_0003;
const INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;

/// Turns faults in title code into Windows exceptions, as Windows does:
/// the title's own handlers see them, and an unhandled one is reported
/// with the title's stack.
/// First-hit probes (`VICTORY_PROBE=<offset>[,<offset>..]`, hex offsets into
/// the executable): each byte is replaced by `int3`; the first time a
/// thread reaches it, the byte comes back and the hit is logged.
static PROBES: OnceLock<Vec<(usize, u8)>> = OnceLock::new();

fn set_probes() {
    let Some(base) = crate::runtime::main_image() else { return };
    let wanted: Vec<usize> = std::env::var("VICTORY_PROBE")
        .unwrap_or_default()
        .split(',')
        .filter_map(|t| usize::from_str_radix(t.trim().trim_start_matches("0x"), 16).ok())
        .map(|offset| base + offset)
        .collect();
    let probes = wanted
        .into_iter()
        .filter_map(|at| {
            let page = at & !4095;
            // SAFETY: a byte of the title's code, made writable for the patch.
            unsafe {
                if libc::mprotect(page as *mut c_void, 4096, libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC) != 0 {
                    return None;
                }
                let was = (at as *const u8).read();
                (at as *mut u8).write(0xcc);
                Some((at, was))
            }
        })
        .collect::<Vec<_>>();
    if !probes.is_empty() {
        tracing::info!(count = probes.len(), "probes set");
    }
    let _ = PROBES.set(probes);
}

/// A probe's `int3` at `at`: its byte back, the hit noted. True if it was one.
fn probe_hit(at: u64) -> bool {
    let Some(&(_, was)) = PROBES.get().and_then(|p| p.iter().find(|p| p.0 as u64 == at)) else {
        return false;
    };
    // SAFETY: the probe's byte, writable since `set_probes`.
    unsafe { (at as *mut u8).write(was) };
    let line = format!("VIctory probe hit: {} on thread {}\n", describe(at), super::current_thread_id());
    // SAFETY: a plain write to standard error (safe in a signal handler).
    unsafe { libc::write(2, line.as_ptr().cast(), line.len()) };
    true
}

pub fn install_signal_handlers() {
    set_probes();
    for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGILL, libc::SIGTRAP, libc::SIGFPE] {
        // SAFETY: a handler of ours, with its information.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Moves the faulting thread to [`dispatch_fault`] with its registers in a
/// [`Fault`] below its stack: the dispatch then runs on the thread itself,
/// outside the signal. A fault outside title code is left to macOS.
extern "C" fn on_signal(signal: i32, info: *mut libc::siginfo_t, uc: *mut c_void) {
    // SAFETY: the kernel's context for this signal.
    let mc = unsafe { &mut *(*uc.cast::<libc::ucontext_t>()).uc_mcontext.cast::<MachineContext>() };
    let mut rip = mc.ss[16];
    if victory_loader::image_containing(rip as usize).is_none() {
        // SAFETY: the default action: the fault happens again and ends us.
        unsafe { libc::signal(signal, libc::SIG_DFL) };
        let text = format!("signal {signal} at {rip:#x}, outside the title's code");
        victory_loader::log_crash(&text);
        return;
    }
    let (code, info_words): (u32, Vec<u64>) = match signal {
        libc::SIGSEGV | libc::SIGBUS => {
            // SAFETY: the kernel's information for this signal.
            let address = unsafe { (*info).si_addr() } as u64;
            let kind = if address == rip { 8 } else { u64::from(mc.err & 2 != 0) };
            (ACCESS_VIOLATION, vec![kind, address])
        }
        libc::SIGILL => (ILLEGAL_INSTRUCTION, Vec::new()),
        libc::SIGFPE => (INTEGER_DIVIDE_BY_ZERO, Vec::new()),
        _ => {
            // After `int3`: Windows reports the instruction itself.
            // SAFETY: the byte before the resume address, in title code.
            if unsafe { *((rip - 1) as *const u8) } == 0xcc {
                rip -= 1;
            }
            if probe_hit(rip) {
                mc.ss[16] = rip;
                return;
            }
            (BREAKPOINT, vec![0])
        }
    };
    let rsp = mc.ss[7];
    let at = (rsp - 512 - size_of::<Fault>() as u64) & !15;
    // SAFETY: below the thread's stack pointer, which nothing uses.
    let fault = unsafe { &mut *(at as *mut Fault) };
    // SAFETY: plain data.
    fault.context = unsafe { std::mem::zeroed() };
    for (windows, mac) in FROM_MAC.iter().enumerate() {
        fault.context.gpr[windows] = mc.ss[*mac];
    }
    fault.context.rip = rip;
    fault.context.eflags = mc.ss[17] as u32;
    fault.context.flags = 0x10_000f;
    fault.context.segments = [0x33, 0x2b, 0x2b, 0x53, 0x2b, 0x2b];
    fault.context.mxcsr = u32::from_le_bytes(mc.fs[32..36].try_into().expect("4 bytes"));
    for i in 0..16 {
        let at = 168 + 16 * i;
        let lo = u64::from_le_bytes(mc.fs[at..at + 8].try_into().expect("8 bytes"));
        let hi = u64::from_le_bytes(mc.fs[at + 8..at + 16].try_into().expect("8 bytes"));
        fault.context.xmm[i] = [lo, hi];
    }
    fault.record = ExceptionRecord {
        code,
        flags: 0,
        record: std::ptr::null_mut(),
        address: rip,
        count: info_words.len() as u32,
        info: [0; 15],
    };
    fault.record.info[..info_words.len()].copy_from_slice(&info_words);
    // As if `dispatch_fault(fault)` were called: no return address to use.
    // SAFETY: just below the fault, still on the thread's stack.
    unsafe { ((at - 8) as *mut u64).write(0) };
    mc.ss[7] = at - 8;
    mc.ss[16] = dispatch_fault as usize as u64;
    mc.ss[2] = at; // rcx: the first argument
    mc.ss[17] &= !0x400; // the direction flag clear, as at any call
}

/// Dispatches a fault on its own thread: resumes where a handler says, or
/// reports it.
extern "win64" fn dispatch_fault(fault: &mut Fault) -> ! {
    let emulated = fault.record.code == ILLEGAL_INSTRUCTION
        // SAFETY: the fault's own context.
        && unsafe { victory_loader::emulate_console_instruction((&raw mut fault.context).cast()) };
    if emulated || dispatch(&mut fault.record, &mut fault.context) {
        // SAFETY: the context a handler chose to continue with.
        unsafe { victory_restore_context(&fault.context) }
    }
    unhandled(&fault.record, &fault.context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_and_restore_round_trip() {
        let mut context: Context = unsafe { std::mem::zeroed() };
        let mut passes = 0u32;
        let passes_at = &raw mut passes;
        // SAFETY: we capture and later restore our own frame, which is
        // still live; `passes` is read through a volatile pointer.
        unsafe {
            RtlCaptureContext(&mut context);
            let n = passes_at.read_volatile() + 1;
            passes_at.write_volatile(n);
            if n == 1 {
                victory_restore_context(&context);
            }
        }
        assert_eq!(passes, 2, "restoring the context resumed after the capture");
    }

    #[test]
    fn our_bridges_are_in_our_table() {
        // SAFETY: the address of our own label.
        let pc = unsafe { &raw const victory_consolidate_return } as u64;
        let (base, f) = lookup(pc).expect("found");
        assert_eq!(base as usize, our_table().base);
        // SAFETY: an entry of our table.
        assert_eq!(unsafe { (*f).begin } as u64 + base, pc);
        let _ = (RaiseException as usize, RtlUnwindEx as usize, RtlUnwind as usize, RtlCaptureStackBackTrace as usize);
    }
}
