// Checks the macOS facts the native port rests on, in an x86_64 process
// under Rosetta (the title is x86-64 code, so the port runs there too).
// Build and run: mac/probe.sh
#import <Metal/Metal.h>
#include <mach/mach.h>
#include <pthread.h>
#include <cpuid.h>
#include <immintrin.h>
#include <signal.h>
#include <sys/wait.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

static int failures;
#define CHECK(ok, ...) do { printf("%s ", (ok) ? "ok  " : "FAIL"); printf(__VA_ARGS__); printf("\n"); if (!(ok)) failures++; } while (0)

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

// Write guard: a fault on a read-only 4 KiB page records the address and
// opens the page, the way the engine tracks what the title writes.
static volatile uintptr_t faulted;
static volatile long faults;
static void on_fault(int sig, siginfo_t *info, void *ctx) {
    (void)sig; (void)ctx;
    uintptr_t page = (uintptr_t)info->si_addr & ~(uintptr_t)4095;
    faulted = (uintptr_t)info->si_addr;
    faults++;
    mprotect((void *)page, 4096, PROT_READ | PROT_WRITE);
}

static void pages_and_guards(void) {
    printf("page size: getpagesize %d, vm_page_size %lu\n", getpagesize(), (unsigned long)vm_page_size);
    struct sigaction sa = {0};
    sa.sa_sigaction = on_fault;
    sa.sa_flags = SA_SIGINFO;
    sigaction(SIGSEGV, &sa, 0);
    sigaction(SIGBUS, &sa, 0);

    uint8_t *m = mmap(0, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    memset(m, 0, 1 << 20);
    CHECK(mprotect(m + 4096, 4096, PROT_READ) == 0, "mprotect of one 4 KiB page");
    faults = 0;
    m[0] = 1;
    m[8192] = 1;
    CHECK(faults == 0, "writes beside the guarded page do not fault (%ld faults)", faults);
    m[4096 + 5] = 7;
    CHECK(faults == 1 && faulted == (uintptr_t)(m + 4096 + 5) && m[4096 + 5] == 7,
          "write to guarded page faults once at the exact address and lands");

    enum { N = 20000 };
    faults = 0;
    double t0 = now();
    for (int i = 0; i < N; i++) {
        uint8_t *p = m + 4096 * (i % 256);
        mprotect(p, 4096, PROT_READ);
        p[1] = (uint8_t)i;
    }
    double us = (now() - t0) / N * 1e6;
    CHECK(faults == N, "%d guard faults, %.2f us per protect + fault + reopen", N, us);
    munmap(m, 1 << 20);
}

// The title is built for Zen 2 and uses AVX2 without asking. Rosetta
// translates AVX2; CPUID shows it only with ROSETTA_ADVERTISE_AVX=1.
__attribute__((target("avx2"))) static void masked_store(int32_t *p) {
    __m256i mask = _mm256_set_epi32(0, 0, 0, 0, 0, 0, 0, -1);
    _mm256_maskstore_epi32(p, mask, _mm256_set1_epi32(7));
}
static void cpu(void) {
    unsigned a, b, c, d;
    __get_cpuid(1, &a, &b, &c, &d);
    int avx = c >> 28 & 1, fma = c >> 12 & 1, f16c = c >> 29 & 1;
    __get_cpuid_count(7, 0, &a, &b, &c, &d);
    int avx2 = b >> 5 & 1, bmi2 = b >> 8 & 1;
    printf("info CPUID: AVX %d AVX2 %d FMA %d F16C %d BMI2 %d (ROSETTA_ADVERTISE_AVX=%s)\n", avx, avx2, fma, f16c, bmi2,
           getenv("ROSETTA_ADVERTISE_AVX") ? getenv("ROSETTA_ADVERTISE_AVX") : "unset");
    int32_t *m = mmap(0, 8192, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    masked_store(m);
    CHECK(m[0] == 7 && m[1] == 0, "AVX2 masked store runs");

    // A masked store into a write-guarded page, in a child: under Wine this
    // stopped the title with a Rosetta error.
    pid_t child = fork();
    if (child == 0) {
        mprotect(m, 4096, PROT_READ);
        faults = 0;
        masked_store(m + 4);
        _exit(faults == 1 && m[4] == 7 ? 0 : 3);
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("info AVX2 masked store into a guarded page: %s\n",
           WIFEXITED(status) && WEXITSTATUS(status) == 0 ? "fault handled, store landed"
           : WIFSIGNALED(status) ? "process killed (Rosetta cannot resume it)" : "handled wrongly");
}

// PE images are mapped at their preferred base when it is free.
static void fixed_address(void) {
    void *want = (void *)0x140000000;
    void *got = mmap(want, 128 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON | MAP_FIXED, -1, 0);
    CHECK(got == want, "128 MiB at the PE default base 0x140000000 (got %p)", got);
    if (got != MAP_FAILED) munmap(got, 128 << 20);
    want = (void *)0x7ff600000000;
    got = mmap(want, 1 << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON | MAP_FIXED, -1, 0);
    CHECK(got == want, "1 MiB at a high Windows-style address 0x7ff600000000 (got %p)", got);
    if (got != MAP_FAILED) munmap(got, 1 << 20);
}

// Windows x64 code reads its thread block through gs: (0x30 self,
// 0x58 TLS array). On macOS gs: points at the pthread TSD array; slots 6
// (0x30) and 11 (0x58) are left unused by libSystem.
static uint64_t gs_read(int off) {
    uint64_t v;
    switch (off) {
    case 0x30: __asm__ volatile("movq %%gs:0x30, %0" : "=r"(v)); break;
    case 0x58: __asm__ volatile("movq %%gs:0x58, %0" : "=r"(v)); break;
    default: __asm__ volatile("movq %%gs:0x0, %0" : "=r"(v)); break;
    }
    return v;
}
static void gs_write(uint64_t teb, uint64_t tls) {
    __asm__ volatile("movq %0, %%gs:0x30" ::"r"(teb));
    __asm__ volatile("movq %0, %%gs:0x58" ::"r"(tls));
}
static void *gs_thread(void *arg) {
    uint64_t base = (uint64_t)arg;
    gs_write(base, base + 8);
    usleep(1000);
    void *keep = malloc(1000);
    free(keep);
    return (void *)(uintptr_t)(gs_read(0x30) == base && gs_read(0x58) == base + 8);
}
static void thread_block(void) {
    uint64_t self = gs_read(0);
    CHECK(self == (uint64_t)pthread_self(), "gs:0 is pthread_self (%llx)", (unsigned long long)self);
    gs_write(0x1111000, 0x2222000);
    char buf[64];
    snprintf(buf, sizeof buf, "%d", 42);
    pthread_t t[4];
    for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, gs_thread, (void *)(uintptr_t)(0x10000000 * (i + 1)));
    int all = 1;
    for (int i = 0; i < 4; i++) { void *r; pthread_join(t[i], &r); all &= r != 0; }
    CHECK(gs_read(0x30) == 0x1111000 && gs_read(0x58) == 0x2222000 && all,
          "gs:0x30 and gs:0x58 hold per-thread values across libc calls and threads");
}

// Unified memory: the GPU reads and writes memory we mapped ourselves
// (as title memory would be), with no copy.
static void unified_memory(void) {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    CHECK(dev != nil, "Metal device: %s, unified %d, max working set %.2f GB, max buffer %.2f GB", dev.name.UTF8String,
          dev.hasUnifiedMemory, dev.recommendedMaxWorkingSetSize / 1e9, dev.maxBufferLength / 1e9);
    size_t len = 64 << 20;
    uint32_t *mem = mmap(0, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    for (size_t i = 0; i < len / 4; i++) mem[i] = (uint32_t)i;
    id<MTLBuffer> buf = [dev newBufferWithBytesNoCopy:mem length:len options:MTLResourceStorageModeShared deallocator:nil];
    CHECK(buf != nil && buf.contents == mem, "no-copy buffer over our own mapping (contents %p, gpuAddress 0x%llx)",
          buf.contents, (unsigned long long)buf.gpuAddress);
    id<MTLBuffer> odd = [dev newBufferWithBytesNoCopy:(uint8_t *)mem + 4096 length:len - 16384
                                              options:MTLResourceStorageModeShared deallocator:nil];
    printf("info no-copy buffer at a 4 KiB (not 16 KiB) aligned address: %s\n", odd ? "accepted" : "rejected");

    NSError *err = nil;
    id<MTLLibrary> lib = [dev newLibraryWithSource:
        @"kernel void add(device uint *m [[buffer(0)]], uint i [[thread_position_in_grid]]) { m[i] += 1; }"
                                           options:nil error:&err];
    id<MTLComputePipelineState> pso = [dev newComputePipelineStateWithFunction:[lib newFunctionWithName:@"add"] error:&err];
    id<MTLCommandQueue> q = [dev newCommandQueue];
    double t0 = now();
    id<MTLCommandBuffer> cb = [q commandBuffer];
    id<MTLComputeCommandEncoder> enc = [cb computeCommandEncoder];
    [enc setComputePipelineState:pso];
    [enc setBuffer:buf offset:0 atIndex:0];
    [enc dispatchThreads:MTLSizeMake(len / 4, 1, 1) threadsPerThreadgroup:MTLSizeMake(256, 1, 1)];
    [enc endEncoding];
    [cb commit];
    [cb waitUntilCompleted];
    double ms = (now() - t0) * 1e3;
    int right = 1;
    for (size_t i = 0; i < len / 4; i += 4099) right &= mem[i] == (uint32_t)i + 1;
    CHECK(right, "GPU wrote 64 MiB of our mapping in place, CPU sees it with no copy (%.2f ms)", ms);
}

int main(void) {
#if !defined(__x86_64__)
    printf("FAIL build this probe with -arch x86_64\n");
    return 1;
#endif
    @autoreleasepool {
        pages_and_guards();
        cpu();
        fixed_address();
        thread_block();
        unified_memory();
    }
    printf("%s (%d failed)\n", failures ? "PROBE FAILED" : "PROBE PASSED", failures);
    return failures != 0;
}
