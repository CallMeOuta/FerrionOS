#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern uintptr_t redox_cur_thrfd_v0(void);
extern uintptr_t redox_dup_v1(uintptr_t fd, const char *buf, uintptr_t len);
extern uintptr_t redox_read_v1(uintptr_t fd, void *buf, uintptr_t len);
extern uintptr_t redox_write_v1(uintptr_t fd, const void *buf, uintptr_t len);
extern uintptr_t redox_close_v1(uintptr_t fd);

#define USER_END ((uintptr_t)1 << 47)
#define SYS_YIELD 158
#define SYS_CLOCK_GETTIME 265
#define MARK 0x5ec0de

static int failures;

static void report(int ok, const char *name, const char *why) {
    if (ok) {
        printf("foreign-call-probe: pass %s\n", name);
    } else {
        printf("foreign-call-probe: FAIL %s: %s\n", name, why);
        failures++;
    }
    fflush(stdout);
}

static long raw_err(uintptr_t r) { return r > (uintptr_t)-4096 ? -(long)r : 0; }

static long open_hook(uintptr_t thread_fd) {
    uintptr_t r = redox_dup_v1(thread_fd, "foreign-call", 12);
    return raw_err(r) ? -raw_err(r) : (long)r;
}

static long set_hook(long fd, uintptr_t entry, uintptr_t start, uintptr_t end) {
    uintptr_t words[3] = {entry, start, end};
    uintptr_t r = redox_write_v1((uintptr_t)fd, words, sizeof words);
    return raw_err(r) ? -raw_err(r) : (long)r;
}

static int get_hook(long fd, uintptr_t out[3]) {
    uintptr_t r = redox_read_v1((uintptr_t)fd, out, 3 * sizeof(uintptr_t));
    return raw_err(r) ? -1 : (int)r;
}

struct seen {
    uint64_t gpr[16];
    uint64_t rflags;
    uint64_t xmm0[2];
    uint64_t fsbase, gsbase;
    uint64_t entries;
};
struct seen seen;
uint64_t sent_rsp;

extern char fc_entry[], fc_call[];
__asm__(
    ".text\n"
    ".globl fc_entry\n"
    "fc_entry:\n"
    "    mov [rip + seen + 0], rax\n"
    "    mov [rip + seen + 8], rbx\n"
    "    mov [rip + seen + 16], rcx\n"
    "    mov [rip + seen + 24], rdx\n"
    "    mov [rip + seen + 32], rsi\n"
    "    mov [rip + seen + 40], rdi\n"
    "    mov [rip + seen + 48], rbp\n"
    "    mov [rip + seen + 56], rsp\n"
    "    mov [rip + seen + 64], r8\n"
    "    mov [rip + seen + 72], r9\n"
    "    mov [rip + seen + 80], r10\n"
    "    mov [rip + seen + 88], r11\n"
    "    mov [rip + seen + 96], r12\n"
    "    mov [rip + seen + 104], r13\n"
    "    mov [rip + seen + 112], r14\n"
    "    mov [rip + seen + 120], r15\n"
    "    pushfq\n"
    "    pop qword ptr [rip + seen + 128]\n"
    "    movq [rip + seen + 136], xmm0\n"
    "    movhps [rip + seen + 144], xmm0\n"
    "    push rax\n"
    "    rdfsbase rax\n"
    "    mov [rip + seen + 152], rax\n"
    "    rdgsbase rax\n"
    "    mov [rip + seen + 160], rax\n"
    "    pop rax\n"
    "    inc qword ptr [rip + seen + 168]\n"
    "    mov rax, 0x5ec0de\n"
    "    jmp rcx\n"
    ".globl fc_call\n"
    "fc_call:\n"
    "    push rbx\n push rbp\n push r12\n push r13\n push r14\n push r15\n"
    "    mov r11, rdi\n"
    "    mov rax, 0x5151515151515151\n movq xmm0, rax\n"
    "    mov rax, 0x5252525252525252\n pinsrq xmm0, rax, 1\n"
    "    mov rax, 0x1100\n mov rbx, 0x1101\n mov rdx, 0x1103\n mov rsi, 0x1104\n"
    "    mov rdi, 0x1105\n mov rbp, 0x1106\n mov r8, 0x1108\n mov r9, 0x1109\n"
    "    mov r10, 0x110a\n mov r12, 0x110c\n mov r13, 0x110d\n mov r14, 0x110e\n"
    "    mov r15, 0x110f\n"
    "    mov [rip + sent_rsp], rsp\n"
    "    call r11\n"
    "    pop r15\n pop r14\n pop r13\n pop r12\n pop rbp\n pop rbx\n"
    "    ret\n");

extern char __executable_start[], etext[];
typedef uint64_t (*page_fn)(void);
static unsigned char *page;

static void make_page(void) {
    page = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        perror("mmap");
        exit(100);
    }
    page[0] = 0x0f;
    page[1] = 0x05;
    page[2] = 0xc3;
    if (mprotect(page, 4096, PROT_READ | PROT_EXEC) != 0) {
        perror("mprotect");
        exit(100);
    }
}

static uint64_t call_page(void) { return ((uint64_t(*)(void *))fc_call)(page); }

static uint64_t rdfs(void) { uint64_t v; __asm__ volatile("rdfsbase %0" : "=r"(v)); return v; }
static uint64_t rdgs(void) { uint64_t v; __asm__ volatile("rdgsbase %0" : "=r"(v)); return v; }

static const char *one_foreign(char *why, size_t len) {
    uint64_t before = seen.entries;
    uint64_t fs = rdfs(), gs = rdgs();
    uint64_t r = call_page();
    if (seen.entries != before + 1) return "the entry was not reached";
    if (r != MARK) return "the stub's result did not come back";
    static const int idx[] = {0, 1, 3, 4, 5, 6, 8, 9, 10, 12, 13, 14, 15};
    for (unsigned i = 0; i < sizeof idx / sizeof idx[0]; i++) {
        if (seen.gpr[idx[i]] != 0x1100u + idx[i]) {
            snprintf(why, len, "register %d was %#llx", idx[i], (unsigned long long)seen.gpr[idx[i]]);
            return why;
        }
    }
    if (seen.gpr[2] != (uint64_t)(uintptr_t)page + 2) return "RCX is not the return address";
    if (seen.gpr[7] != sent_rsp - 8) return "RSP changed";
    if (seen.gpr[11] != seen.rflags) return "R11 does not hold the flags";
    if (seen.xmm0[0] != 0x5151515151515151 || seen.xmm0[1] != 0x5252525252525252) return "XMM0 changed";
    if (seen.fsbase != fs || seen.gsbase != gs) return "FS or GS base changed";
    return NULL;
}

static uint64_t native_yield_from_page(void) {
    uint64_t r;
    __asm__ volatile("call %1" : "=a"(r) : "r"(page), "0"((uint64_t)SYS_YIELD) : "rcx", "r11", "memory");
    return r;
}

static volatile uintptr_t other_thread_fd;
static volatile int other_go, other_done;
static volatile uint64_t other_result, other_entries_delta;

static void *other_thread(void *arg) {
    (void)arg;
    other_thread_fd = redox_cur_thrfd_v0();
    while (!other_go) sched_yield();
    long fd = open_hook(redox_cur_thrfd_v0());
    uintptr_t w[3] = {1, 1, 1};
    if (fd >= 0) get_hook(fd, w), redox_close_v1(fd);
    uint64_t before = seen.entries;
    other_result = native_yield_from_page();
    other_entries_delta = seen.entries - before;
    other_go = (w[0] | w[1] | w[2]) == 0 ? 2 : 3;
    other_done = 1;
    return NULL;
}

static double now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1e9 + ts.tv_nsec;
}

static volatile uint64_t sig_count;
static void on_usr1(int sig) { (void)sig; sig_count++; }
static volatile int stop_signals;
static void *signaller(void *arg) {
    pthread_t target = *(pthread_t *)arg;
    while (!stop_signals) {
        pthread_kill(target, SIGUSR1);
        for (volatile int i = 0; i < 2000; i++) {}
    }
    return NULL;
}


static void bench(int registered) {
    enum { N = 200000 };
    struct { int64_t sec; int32_t nsec; } ts;
    double t0 = now_ns();
    for (int i = 0; i < N; i++) {
        uint64_t r;
        __asm__ volatile("syscall" : "=a"(r) : "a"(SYS_CLOCK_GETTIME), "D"(1), "S"(&ts)
                         : "rcx", "r11", "memory");
    }
    double t1 = now_ns();
    printf("foreign-call-probe: bench native-clock_gettime %.1f ns/call\n", (t1 - t0) / N);
    if (registered) {
        double t2 = now_ns();
        for (int i = 0; i < N; i++) call_page();
        double t3 = now_ns();
        printf("foreign-call-probe: bench foreign-round-trip %.1f ns/call\n", (t3 - t2) / N);
    }
    fflush(stdout);
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (argc == 2 && strcmp(argv[1], "--after-exec") == 0) {
        long fd = open_hook(redox_cur_thrfd_v0());
        uintptr_t w[3] = {1, 1, 1};
        int ok = fd >= 0 && get_hook(fd, w) == 24 && (w[0] | w[1] | w[2]) == 0;
        report(ok, "cleared-by-exec", "the registration survived exec");
        return failures;
    }
    if (argc == 2 && strcmp(argv[1], "--bench-only") == 0) {
        bench(0);
        return 0;
    }

    uintptr_t start = (uintptr_t)__executable_start, end = (uintptr_t)etext;
    uintptr_t entry = (uintptr_t)fc_entry;
    make_page();

    long fd = open_hook(redox_cur_thrfd_v0());
    if (fd < 0) {
        char why[96];
        snprintf(why, sizeof why, "opening the thread's foreign-call handle failed: %s", strerror((int)-fd));
        report(0, "handle", why);
        bench(0);
        return failures;
    }
    report(1, "handle", "");

    report(set_hook(fd, entry, end, start) == -EINVAL, "refuse-empty-range", "accepted end < start");
    report(set_hook(fd, entry, start, USER_END + 4096) == -EINVAL, "refuse-kernel-range",
           "accepted a range past user memory");
    report(set_hook(fd, (uintptr_t)page, start, end) == -EINVAL, "refuse-entry-outside",
           "accepted an entry outside the range");

    pthread_t other;
    pthread_create(&other, NULL, other_thread, NULL);
    while (!other_thread_fd) sched_yield();
    long ofd = open_hook(other_thread_fd);
    report(ofd >= 0 && set_hook(ofd, entry, start, end) == -EPERM, "refuse-other-thread",
           "another thread's registration was accepted");
    if (ofd >= 0) redox_close_v1(ofd);

    if (set_hook(fd, entry, start, end) != 24) {
        report(0, "register", "a valid registration was refused");
        return failures;
    }
    uintptr_t w[3] = {0};
    report(get_hook(fd, w) == 24 && w[0] == entry && w[1] == start && w[2] == end, "read-back",
           "the handle does not read back the registration");

    char why[96];
    const char *bad = one_foreign(why, sizeof why);
    report(!bad, "registers-intact", bad);

    uint64_t before = seen.entries;
    struct { int64_t sec; int32_t nsec; } ts = {0, 0};
    uint64_t r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(SYS_CLOCK_GETTIME), "D"(1), "S"(&ts) : "rcx", "r11", "memory");
    report(r == 0 && ts.sec > 0 && seen.entries == before, "native-inside-range",
           "a call inside the range was not dispatched");

    other_go = 1;
    while (!other_done) sched_yield();
    pthread_join(other, NULL);
    report(other_go == 2 && other_entries_delta == 0 && other_result == 0, "other-thread-unaffected",
           "an unregistered thread was redirected or started registered");

    struct sigaction sa = {0};
    sa.sa_handler = on_usr1;
    sigaction(SIGUSR1, &sa, NULL);
    pthread_t self = pthread_self(), sig_thread;
    pthread_create(&sig_thread, NULL, signaller, &self);
    const char *sig_bad = NULL;
    double until = now_ns() + 5e9;
    unsigned long calls = 0;
    while (!sig_bad && (calls < 20000 || (sig_count < 200 && now_ns() < until))) {
        sig_bad = one_foreign(why, sizeof why);
        calls++;
    }
    stop_signals = 1;
    pthread_join(sig_thread, NULL);
    if (!sig_bad && sig_count < 200) sig_bad = "fewer than 200 signals arrived during the loop";
    report(!sig_bad, "signals-at-boundary", sig_bad);
    printf("foreign-call-probe: %llu signals handled during %lu foreign calls\n",
           (unsigned long long)sig_count, calls);

    bench(1);

    report(set_hook(fd, 0, 0, 0) == 24, "unregister", "three zeros were refused");
    before = seen.entries;
    r = native_yield_from_page();
    report(r == 0 && seen.entries == before, "unregistered-dispatches", "the call still reached the entry");

    set_hook(fd, entry, start, end);
    fflush(stdout);
    char *args[] = {argv[0], "--after-exec", NULL};
    pid_t child = fork();
    if (child == 0) {
        long cfd = open_hook(redox_cur_thrfd_v0());
        if (cfd < 0 || set_hook(cfd, entry, start, end) != 24) _exit(90);
        execv("/usr/bin/foreign-call-probe", args);
        _exit(91);
    }
    int status = 0;
    waitpid(child, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status) >= 90) {
        report(0, "cleared-by-exec", "the exec child did not run");
    } else {
        failures += WEXITSTATUS(status);
    }
    printf("foreign-call-probe: %s (%d failures)\n", failures ? "FAIL" : "PASS", failures);
    return failures;
}
