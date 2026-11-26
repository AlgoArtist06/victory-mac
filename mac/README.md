# VIctory on macOS (Apple Silicon)

This fork works toward a native macOS port of VIctory.
Native means no Wine: the engine is a macOS program, and the title's GPU work goes to Metal.
The title's own code is x86-64, so the engine runs as an x86-64 program under Rosetta 2.

The main reason for a native port is unified memory.
On a PC, the CPU and the graphics card have separate memory, so the engine keeps two copies of the title's memory and copies changes every frame.
An Apple Silicon Mac has one memory, like the console.
Metal can use the title's memory in place, so the copy loop and most write tracking can go away.

## Measured facts

`mac/probe.sh` checks these on the machine it runs on.
Results on an Apple M2 with 8 GB of memory, macOS 27, under Rosetta:

| Check | Result |
|---|---|
| Page size for x86-64 programs | 4 KiB (the native page is 16 KiB, but Rosetta processes get 4 KiB) |
| Write guard on one 4 KiB page | Works: the fault comes at the exact address, and the write lands after the page opens |
| Cost of one guard (protect, fault, reopen) | About 46 us, so the port must avoid per-frame write tracking |
| AVX2 (the title is built for Zen 2) | Rosetta runs it; CPUID reports it only with `ROSETTA_ADVERTISE_AVX=1` |
| AVX2 masked store into a guarded page | Natively the fault is handled and the store lands (under Wine, Rosetta stops the process instead) |
| Map at the PE default base 0x140000000 | Works |
| Thread block through `gs:` (0x30 self, 0x58 TLS array) | Works per thread, across libc calls |
| Metal buffer over memory we mapped, with no copy | Works, also at 4 KiB alignment |
| GPU writes 64 MiB of that memory, CPU reads it with no copy | Works (about 11 ms on the M2) |
| Largest GPU working set Metal recommends | 5.73 GB on the 8 GB M2 |
| Largest Metal buffer | 4.29 GB, so title GPU memory needs a few buffers, each with its own offset |

## Proof run under Wine

Wine is used only to prove the title starts on this Mac and to collect data.
It is not the target.

The Windows build cross-compiles on macOS with llvm-mingw (`x86_64-pc-windows-gnullvm`).
Under Wine, `victory run` maps the title and its four DLLs and binds every import with none missing:

| Image | Our runtime | Host (Windows) | Title DLLs |
|---|---|---|---|
| game_scarlett_master.exe | 94 | 421 | 72 |
| libhttpclient.gdk.dll | 27 | 171 | 0 |
| bink2scarlett.dll | 7 | 53 | 0 |
| xboxbvhbuilder.dll | 9 | 48 | 0 |
| amd_fidelityfx_loader_xbox.dll | 4 | 18 | 0 |

The "Host" column is what the native port has to provide itself.

With drawing off (`VICTORY_NO_DRAWS=1`, Series S profile, write watch off), the title ran 7 min 21 s without a crash.
Its frame loop presented 12,874 frames, a locked 30 per second in 403 of the 418 seconds.
The picture is black, so the scene is not known; it is likely an intro or loading phase.
The process's physical footprint grew from 357 MB to 2.1 GB in the first minute, then by about 150 MB a minute to 3.04 GB.
Swap stayed at about 1 GB.
This includes the memory the title allocates for its GPU resources, but not our pipelines, render targets or host GPU copies.

Two problems are Wine's, not the title's:
`GetLogicalDrives` never returns inside this process under this Wine build,
and Wine imitates write watch with read-only pages, so a fault inside an AVX2 masked store stops the process with a Rosetta error.

## Steps

Each step ends with a check that runs on the Mac.

1. Fork, and build the crates that do not depend on Windows for `x86_64-apple-darwin`.
   Check: `victory-core`, `-pe`, `-formats`, `-pipeline`, `-game` and `-shader` build and pass their tests.
2. Probe the platform facts above.
   Check: `mac/probe.sh` passes.
3. Calling convention: functions the title calls use the Windows x64 convention.
   On Windows `extern "system"` is that convention, but on macOS it is the C convention, so title-facing functions become `extern "win64"`.
   Check: the Windows build is unchanged, and the macOS build compiles them.
4. Loader: map PE images ourselves, apply relocations, give the title TLS index 0 through `gs:0x58`, and bind imports.
   Check: `victory run` on macOS prints the same bind table as the Wine run.
   Done: `cargo run --target x86_64-apple-darwin -p victory-loader --example mac_load -- <title folder>` maps the title at 0x140000000 with TLS index 0, binds 72 imports between the title's images, leaves exactly the Wine run's runtime + host imports to provide (515, 198, 60, 57, 22), and a second copy moves all 598,281 relocations correctly.
5. Host layer: the Windows functions in the "Host" column, on POSIX and Mach, plus structured exception handling from the images' unwind tables.
   Check: the title runs with `VICTORY_NO_DRAWS=1` and a null GPU, and its memory is measured.
6. Metal back end over unified memory: title GPU memory is a few no-copy Metal buffers, so a GPU address is the CPU address plus that buffer's offset.
   Check: the first frame of the title draws.
