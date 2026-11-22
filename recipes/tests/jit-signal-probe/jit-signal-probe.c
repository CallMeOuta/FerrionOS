#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifdef __linux__
#define UC_RIP(uc) ((uc)->uc_mcontext.gregs[REG_RIP])
#define UC_RSP(uc) ((uc)->uc_mcontext.gregs[REG_RSP])
#define UC_RAX(uc) ((uc)->uc_mcontext.gregs[REG_RAX])
#define UC_RDX(uc) ((uc)->uc_mcontext.gregs[REG_RDX])
#else
#define UC_RIP(uc) ((uc)->uc_mcontext.rip)
#define UC_RSP(uc) ((uc)->uc_mcontext.rsp)
#define UC_RAX(uc) ((uc)->uc_mcontext.rax)
#define UC_RDX(uc) ((uc)->uc_mcontext.rdx)
#endif

#ifndef MAP_NORESERVE
#define MAP_NORESERVE 0
#endif

#define NAME "jit-signal-probe"

extern char fx_store[], fx_store_end[], fx_load[], fx_load_end[], fx_ud2[], fx_ud2_end[];

enum mode { MODE_NONE, MODE_STORE, MODE_LOAD, MODE_UD2, MODE_CALL };

static volatile int expect_mode;
static volatile int expect_signo;
static void *volatile expect_addr;
static volatile int hits;
static volatile int mismatches;
static void *volatile altstack_lo;
static void *volatile altstack_hi;
static volatile int on_altstack;
static long page_size;
static int failures;

static void report(const char *name, int ok, const char *detail)
{
    if (ok) {
        printf("%s: pass %s\n", NAME, name);
    } else {
        printf("%s: FAIL %s: %s\n", NAME, name, detail);
        failures++;
    }
    fflush(stdout);
}

static void handler(int signo, siginfo_t *info, void *context)
{
    ucontext_t *uc = context;
    char here;
    hits++;
    if (altstack_lo && (void *)&here >= altstack_lo && (void *)&here < altstack_hi) {
        on_altstack = 1;
    }
    if (signo != expect_signo) {
        mismatches++;
    }
    switch (expect_mode) {
    case MODE_STORE:
        if ((uintptr_t)UC_RIP(uc) != (uintptr_t)fx_store || info->si_addr != expect_addr || UC_RAX(uc) != 0x1111) {
            mismatches++;
        }
        UC_RAX(uc) = 0x2222;
        UC_RIP(uc) = (uintptr_t)fx_store_end;
        return;
    case MODE_LOAD:
        if ((uintptr_t)UC_RIP(uc) != (uintptr_t)fx_load || info->si_addr != expect_addr) {
            mismatches++;
        }
        UC_RAX(uc) = 0x3333;
        UC_RIP(uc) = (uintptr_t)fx_load_end;
        return;
    case MODE_UD2:
        if ((uintptr_t)UC_RIP(uc) != (uintptr_t)fx_ud2 || UC_RSP(uc) != UC_RDX(uc)) {
            mismatches++;
        }
        UC_RAX(uc) = 0x4444;
        UC_RIP(uc) += 2;
        return;
    case MODE_CALL: {
        uintptr_t *sp = (uintptr_t *)UC_RSP(uc);
        if ((uintptr_t)UC_RIP(uc) != (uintptr_t)expect_addr || info->si_addr != expect_addr) {
            mismatches++;
        }
        UC_RIP(uc) = sp[0];
        UC_RSP(uc) += 8;
        UC_RAX(uc) = 77;
        return;
    }
    default:
        mismatches++;
        signal(signo, SIG_DFL);
        return;
    }
}

static __attribute__((noinline)) uint64_t do_store(void *target)
{
    uint64_t out;
    __asm__ volatile("mov $0x1111, %%rax\n"
                     ".globl fx_store\n"
                     "fx_store:\n"
                     "movq $1, (%1)\n"
                     ".globl fx_store_end\n"
                     "fx_store_end:\n"
                     "mov %%rax, %0\n"
                     : "=r"(out)
                     : "r"(target)
                     : "rax", "memory");
    return out;
}

static __attribute__((noinline)) uint64_t do_load(void *target)
{
    uint64_t out;
    __asm__ volatile(".globl fx_load\n"
                     "fx_load:\n"
                     "movq (%1), %%rax\n"
                     ".globl fx_load_end\n"
                     "fx_load_end:\n"
                     "mov %%rax, %0\n"
                     : "=r"(out)
                     : "r"(target)
                     : "rax", "memory");
    return out;
}

static __attribute__((noinline)) uint64_t do_ud2(void)
{
    uint64_t out;
    __asm__ volatile("mov $0, %%rax\n"
                     "mov %%rsp, %%rdx\n"
                     ".globl fx_ud2\n"
                     "fx_ud2:\n"
                     "ud2\n"
                     ".globl fx_ud2_end\n"
                     "fx_ud2_end:\n"
                     "mov %%rax, %0\n"
                     : "=r"(out)
                     :
                     : "rax", "rdx", "memory");
    return out;
}

static void expect(int mode, int signo, void *addr)
{
    expect_mode = mode;
    expect_signo = signo;
    expect_addr = addr;
    hits = 0;
    mismatches = 0;
}

static void install(void)
{
    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_sigaction = handler;
    action.sa_flags = SA_SIGINFO | SA_NODEFER | SA_ONSTACK;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, NULL) || sigaction(SIGILL, &action, NULL) || sigaction(SIGBUS, &action, NULL)) {
        printf("%s: FAIL sigaction: %s\n", NAME, strerror(errno));
        exit(1);
    }
}

static void *map(size_t length, int prot)
{
    void *page = mmap(NULL, length, prot, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (page == MAP_FAILED) {
        printf("%s: FAIL mmap: %s\n", NAME, strerror(errno));
        exit(1);
    }
    return page;
}

static void check_store(void)
{
    char detail[160];
    void *page = map(page_size, PROT_READ);
    expect(MODE_STORE, SIGSEGV, page);
    uint64_t rax = do_store(page);
    snprintf(detail, sizeof(detail), "hits %d, mismatches %d, rax %#lx (want 1 hit, rax 0x2222)", hits, mismatches,
             (unsigned long)rax);
    report("segv-store", hits == 1 && mismatches == 0 && rax == 0x2222, detail);
    munmap(page, page_size);
}

static void check_load_none(void)
{
    char detail[160];
    void *page = map(page_size, PROT_NONE);
    expect(MODE_LOAD, SIGSEGV, page);
    uint64_t rax = do_load(page);
    snprintf(detail, sizeof(detail), "hits %d, mismatches %d, rax %#lx: a load from a PROT_NONE page %s", hits,
             mismatches, (unsigned long)rax, hits ? "faulted" : "did not fault");
    report("segv-load-prot-none", hits == 1 && mismatches == 0 && rax == 0x3333, detail);
    munmap(page, page_size);

    page = map(page_size, PROT_READ | PROT_WRITE);
    memset(page, 0x55, page_size);
    if (mprotect(page, page_size, PROT_NONE)) {
        snprintf(detail, sizeof(detail), "mprotect PROT_NONE: %s", strerror(errno));
        report("segv-load-mprotect-none", 0, detail);
    } else {
        expect(MODE_LOAD, SIGSEGV, page);
        rax = do_load(page);
        snprintf(detail, sizeof(detail), "hits %d, mismatches %d, rax %#lx: a load after mprotect(PROT_NONE) %s", hits,
                 mismatches, (unsigned long)rax, hits ? "faulted" : "did not fault");
        report("segv-load-mprotect-none", hits == 1 && mismatches == 0 && rax == 0x3333, detail);
    }
    munmap(page, page_size);
}

static void check_ud2(void)
{
    char detail[160];
    expect(MODE_UD2, SIGILL, NULL);
    uint64_t rax = do_ud2();
    snprintf(detail, sizeof(detail), "hits %d, mismatches %d, rax %#lx (want 1 hit, rax 0x4444)", hits, mismatches,
             (unsigned long)rax);
    report("sigill-ud2", hits == 1 && mismatches == 0 && rax == 0x4444, detail);
}

static void check_nx(void)
{
    char detail[160];
    uint8_t *page = map(page_size, PROT_READ | PROT_WRITE);
    page[0] = 0xc3;
    expect(MODE_CALL, SIGSEGV, page);
    uint64_t result = ((uint64_t (*)(void))page)();
    snprintf(detail, sizeof(detail), "hits %d, mismatches %d, result %lu: a call into a page without PROT_EXEC %s",
             hits, mismatches, (unsigned long)result, hits ? "faulted" : "ran");
    report("segv-exec-nx", hits == 1 && mismatches == 0 && result == 77, detail);
    munmap(page, page_size);
}

static const uint8_t return_42[] = {0xb8, 42, 0, 0, 0, 0xc3};

static void check_wx(void)
{
    char detail[200];
    size_t reserve = 256 * 1024 * 1024;
    void *hint = (void *)(uintptr_t)0x3e0000000000ull;
    uint8_t *base = mmap(hint, reserve, PROT_NONE, MAP_PRIVATE | MAP_ANON | MAP_NORESERVE, -1, 0);
    if (base == MAP_FAILED) {
        snprintf(detail, sizeof(detail), "reserve 256 MiB PROT_NONE: %s", strerror(errno));
        report("wx-reserve", 0, detail);
        return;
    }
    report("wx-reserve", 1, NULL);
    uint8_t *code = base + 64 * page_size;
    size_t length = 4 * page_size;
    if (mmap(code, length, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANON, -1, 0) != code) {
        snprintf(detail, sizeof(detail), "commit with MAP_FIXED: %s", strerror(errno));
        report("wx-commit", 0, detail);
        return;
    }
    memcpy(code, return_42, sizeof(return_42));
    if (mprotect(code, length, PROT_READ | PROT_EXEC)) {
        snprintf(detail, sizeof(detail), "mprotect RX: %s", strerror(errno));
        report("wx-exec", 0, detail);
        return;
    }
    int result = ((int (*)(void))code)();
    snprintf(detail, sizeof(detail), "code returned %d, want 42", result);
    report("wx-exec", result == 42, detail);

    expect(MODE_STORE, SIGSEGV, code);
    uint64_t rax = do_store(code);
    snprintf(detail, sizeof(detail), "hits %d, mismatches %d, rax %#lx: a store into RX code %s", hits, mismatches,
             (unsigned long)rax, hits ? "faulted" : "succeeded");
    report("wx-store-faults", hits == 1 && mismatches == 0 && rax == 0x2222, detail);

    if (mprotect(code, length, PROT_READ | PROT_WRITE)) {
        snprintf(detail, sizeof(detail), "mprotect RW: %s", strerror(errno));
        report("wx-rewrite", 0, detail);
        return;
    }
    code[1] = 43;
    expect(MODE_CALL, SIGSEGV, code);
    uint64_t nx = ((uint64_t (*)(void))code)();
    snprintf(detail, sizeof(detail), "hits %d, result %lu: a call into RW code %s", hits, (unsigned long)nx,
             hits ? "faulted" : "ran");
    report("wx-rw-not-exec", hits == 1 && mismatches == 0 && nx == 77, detail);
    if (mprotect(code, length, PROT_READ | PROT_EXEC)) {
        snprintf(detail, sizeof(detail), "mprotect RX again: %s", strerror(errno));
        report("wx-rewrite", 0, detail);
        return;
    }
    result = ((int (*)(void))code)();
    snprintf(detail, sizeof(detail), "rewritten code returned %d, want 43", result);
    report("wx-rewrite", result == 43, detail);

    if (mmap(code, length, PROT_NONE, MAP_FIXED | MAP_PRIVATE | MAP_ANON, -1, 0) != code) {
        snprintf(detail, sizeof(detail), "decommit with MAP_FIXED PROT_NONE: %s", strerror(errno));
        report("wx-decommit", 0, detail);
        return;
    }
    if (mmap(code, length, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANON, -1, 0) != code) {
        snprintf(detail, sizeof(detail), "recommit: %s", strerror(errno));
        report("wx-decommit", 0, detail);
        return;
    }
    snprintf(detail, sizeof(detail), "recommitted page starts with %#x, want 0", code[0]);
    report("wx-decommit", code[0] == 0 && code[1] == 0, detail);
    munmap(base, reserve);
}

static void *thread_main(void *arg)
{
    (void)arg;
    size_t size = 64 * 1024;
    void *stack = map(size, PROT_READ | PROT_WRITE);
    stack_t alt = {.ss_sp = stack, .ss_size = size, .ss_flags = 0};
    if (sigaltstack(&alt, NULL)) {
        printf("%s: FAIL sigaltstack: %s\n", NAME, strerror(errno));
        return (void *)1;
    }
    altstack_lo = stack;
    altstack_hi = (char *)stack + size;
    on_altstack = 0;
    void *page = map(page_size, PROT_READ);
    expect(MODE_STORE, SIGSEGV, page);
    uint64_t rax = do_store(page);
    altstack_lo = NULL;
    alt.ss_flags = SS_DISABLE;
    sigaltstack(&alt, NULL);
    return (void *)(uintptr_t)(hits == 1 && mismatches == 0 && rax == 0x2222 && on_altstack ? 0 : 2);
}

static void check_thread(void)
{
    char detail[160];
    pthread_t thread;
    void *status;
    if (pthread_create(&thread, NULL, thread_main, NULL) || pthread_join(thread, &status)) {
        report("thread-altstack", 0, "pthread_create or pthread_join failed");
        return;
    }
    snprintf(detail, sizeof(detail), "status %lu, hits %d, mismatches %d, on altstack %d", (unsigned long)(uintptr_t)status,
             hits, mismatches, on_altstack);
    report("thread-altstack", status == NULL, detail);
}

static void check_repeat(void)
{
    char detail[160];
    int rounds = 50, good = 0;
    void *page = map(page_size, PROT_READ);
    struct timespec start, end;
    clock_gettime(CLOCK_MONOTONIC, &start);
    for (int i = 0; i < rounds; i++) {
        expect(MODE_STORE, SIGSEGV, page);
        if (do_store(page) == 0x2222 && hits == 1 && mismatches == 0) {
            good++;
        }
    }
    clock_gettime(CLOCK_MONOTONIC, &end);
    double micros = ((end.tv_sec - start.tv_sec) * 1e9 + (end.tv_nsec - start.tv_nsec)) / 1e3 / rounds;
    printf("%s: fault round trip %.1f us\n", NAME, micros);
    snprintf(detail, sizeof(detail), "%d of %d faults resumed", good, rounds);
    report("segv-repeat", good == rounds, detail);
    munmap(page, page_size);
}

static void check_default_after_handler(void)
{
    char detail[160];
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) {
        expect(MODE_NONE, SIGSEGV, NULL);
        void *page = map(page_size, PROT_READ);
        do_store(page);
        _exit(0);
    }
    int status = 0;
    if (child < 0 || waitpid(child, &status, 0) != child) {
        report("segv-default-after-handler", 0, "fork or waitpid failed");
        return;
    }
    snprintf(detail, sizeof(detail), "child status %#x, want killed by SIGSEGV", status);
    report("segv-default-after-handler", WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV, detail);
}

int main(void)
{
    page_size = sysconf(_SC_PAGESIZE);
    install();
    check_store();
    check_ud2();
    check_nx();
    check_wx();
    check_load_none();
    check_thread();
    check_repeat();
    check_default_after_handler();
    printf("%s: %s\n", NAME, failures ? "FAILED" : "PASS");
    return failures ? 1 : 0;
}
