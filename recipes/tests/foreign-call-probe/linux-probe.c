/*
 * linux-probe: a static Linux program with no C library, built with -nostdlib so that every
 * system call is a raw Linux x86-64 call. Run under `linux-run`, it checks the crossings LC1 of
 * plans/linux-compat.md requires beyond the four hello programs: registers, XMM state and the
 * FS base survive a call; a Linux signal handler gets a Linux frame (siginfo, ucontext with the
 * interrupted registers) on the right stack, sigreturn resumes the interrupted code, a blocked
 * signal waits for its unblock; a thread made by clone with CLONE_SETTLS sees its own FS base
 * and its exit clears the child-tid word; and it prints the cost of a cached call (getpid) and
 * of one that leaves the runtime (getppid). Lines read `linux-probe: pass NAME` or
 * `linux-probe: FAIL NAME`; the exit status is the number of failures.
 */
typedef unsigned long u64;
typedef long i64;

#define SYS_write 1
#define SYS_mmap 9
#define SYS_rt_sigaction 13
#define SYS_rt_sigprocmask 14
#define SYS_rt_sigreturn 15
#define SYS_getpid 39
#define SYS_clone 56
#define SYS_exit 60
#define SYS_kill 62
#define SYS_getppid 110
#define SYS_sigaltstack 131
#define SYS_arch_prctl 158
#define SYS_gettid 186
#define SYS_futex 202
#define SYS_clock_gettime 228
#define SYS_exit_group 231
#define SYS_tgkill 234

static inline i64 sc6(i64 n, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
    register i64 r10 __asm__("r10") = d;
    register i64 r8 __asm__("r8") = e;
    register i64 r9 __asm__("r9") = f;
    i64 r;
    __asm__ volatile("syscall" : "=a"(r) : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return r;
}
#define sc(n, a, b, c) sc6(n, (i64)(a), (i64)(b), (i64)(c), 0, 0, 0)

static u64 slen(const char *s) { u64 n = 0; while (s[n]) n++; return n; }
static void puts1(const char *s) { sc(SYS_write, 1, s, slen(s)); }
static void putnum(u64 v) {
    char b[24]; int i = 23; b[i] = 0;
    do { b[--i] = '0' + v % 10; v /= 10; } while (v);
    puts1(b + i);
}

static int failures;
static void report(int ok, const char *name) {
    puts1(ok ? "linux-probe: pass " : "linux-probe: FAIL ");
    puts1(name);
    puts1("\n");
    failures += !ok;
}

/* Linux's kernel sigaction for x86-64. */
struct ksigaction { void *handler; u64 flags; void *restorer; u64 mask; };
#define SA_SIGINFO 4
#define SA_ONSTACK 0x08000000
#define SA_RESTORER 0x04000000
#define SIGUSR1 10
#define SIGUSR2 12

extern char restore_rt[];
__asm__(".text\n restore_rt:\n mov $15, %eax\n syscall\n hlt\n");

static volatile int got_sig, got_code;
static volatile u64 got_rsp, got_fs, got_uc_rip, got_info_signo;

static u64 rdfs(void) { u64 v; __asm__ volatile("rdfsbase %0" : "=r"(v)); return v; }

static void handler(int sig, void *info, void *ucv) {
    u64 *uc = ucv;
    int *si = info;
    u64 sp;
    __asm__ volatile("mov %%rsp, %0" : "=r"(sp));
    got_sig = sig;
    got_info_signo = si[0];
    got_code = si[2];
    got_rsp = sp;
    got_fs = rdfs();
    /* uc_mcontext starts at offset 40; rip is its 17th word. */
    got_uc_rip = uc[5 + 16];
}

static char altstack[16384] __attribute__((aligned(16)));
static char tls_main[64], tls_child[64];
static volatile int child_tid_word = 1;
static volatile u64 child_fs, child_ran;
static char child_stack[16384] __attribute__((aligned(16)));

/* The child starts on child_stack with RAX 0 and the parent's registers otherwise. */
static void child_main(void) {
    child_fs = rdfs();
    child_ran = 1;
    sc(SYS_exit, 0, 0, 0);
}

static i64 do_clone(u64 flags, void *stack, volatile int *ptid, volatile int *ctid, void *tls) {
    register i64 r10 __asm__("r10") = (i64)ctid;
    register i64 r8 __asm__("r8") = (i64)tls;
    i64 r;
    __asm__ volatile(
        "syscall\n"
        "test %%rax, %%rax\n"
        "jnz 1f\n"
        "call *%[fn]\n"
        "hlt\n"
        "1:\n"
        : "=a"(r)
        : "a"(SYS_clone), "D"(flags), "S"(stack), "d"(ptid), "r"(r10), "r"(r8), [fn] "r"(child_main)
        : "rcx", "r11", "memory");
    return r;
}

static u64 now_ns(void) {
    i64 ts[2];
    sc(SYS_clock_gettime, 1, ts, 0);
    return (u64)ts[0] * 1000000000ul + (u64)ts[1];
}

void probe_main(void) {
    puts1("hello from linux-probe\n");

    /* FS base: set, read back, survives a call. */
    sc(SYS_arch_prctl, 0x1002 /* ARCH_SET_FS */, tls_main, 0);
    u64 fs = 0;
    sc(SYS_arch_prctl, 0x1003 /* ARCH_GET_FS */, &fs, 0);
    report(fs == (u64)tls_main && rdfs() == (u64)tls_main, "fs-base");

    /* Callee-saved registers and XMM state across a call. */
    {
        u64 out[8];
        __asm__ volatile(
            "push %%rbx\n push %%rbp\n push %%r12\n push %%r13\n push %%r14\n push %%r15\n"
            "mov $0x2201, %%rbx\n mov $0x2202, %%rbp\n mov $0x2203, %%r12\n mov $0x2204, %%r13\n"
            "mov $0x2205, %%r14\n mov $0x2206, %%r15\n"
            "mov $0x7777777777777777, %%rax\n movq %%rax, %%xmm7\n pinsrq $1, %%rax, %%xmm7\n"
            "mov $39, %%eax\n syscall\n"
            "mov %%rbx, 0(%%rdi)\n mov %%rbp, 8(%%rdi)\n mov %%r12, 16(%%rdi)\n mov %%r13, 24(%%rdi)\n"
            "mov %%r14, 32(%%rdi)\n mov %%r15, 40(%%rdi)\n movq %%xmm7, 48(%%rdi)\n pextrq $1, %%xmm7, 56(%%rdi)\n"
            "pop %%r15\n pop %%r14\n pop %%r13\n pop %%r12\n pop %%rbp\n pop %%rbx\n"
            :
            : "D"(out)
            : "rax", "rcx", "r11", "xmm7", "memory");
        int ok = out[6] == 0x7777777777777777ul && out[7] == 0x7777777777777777ul;
        for (int i = 0; i < 6; i++) ok &= out[i] == 0x2201ul + i;
        report(ok, "registers-and-xmm");
    }

    /* A handler gets a Linux frame. */
    struct ksigaction sa = {handler, SA_SIGINFO | SA_RESTORER, restore_rt, 0};
    report(sc6(SYS_rt_sigaction, SIGUSR1, (i64)&sa, 0, 8, 0, 0) == 0, "sigaction");
    i64 pid = sc(SYS_getpid, 0, 0, 0);
    sc(SYS_kill, pid, SIGUSR1, 0);
    report(got_sig == SIGUSR1 && got_info_signo == SIGUSR1 && got_code == 0 /* SI_USER */, "signal-frame");
    report(got_fs == (u64)tls_main, "handler-fs");
    report(got_uc_rip != 0, "ucontext-rip");

    /* Blocked until unblocked. */
    got_sig = 0;
    u64 set = 1ul << (SIGUSR1 - 1), old = 0;
    sc6(SYS_rt_sigprocmask, 0 /* SIG_BLOCK */, (i64)&set, (i64)&old, 8, 0, 0);
    sc(SYS_tgkill, pid, sc(SYS_gettid, 0, 0, 0), SIGUSR1);
    int waited = got_sig == 0;
    sc6(SYS_rt_sigprocmask, 1 /* SIG_UNBLOCK */, (i64)&set, 0, 8, 0, 0);
    report(waited && got_sig == SIGUSR1 && got_code == -6 /* SI_TKILL */, "blocked-then-delivered");

    /* The alternate stack. */
    struct { void *sp; int flags; int pad; u64 size; } ss = {altstack, 0, 0, sizeof altstack};
    sc(SYS_sigaltstack, &ss, 0, 0);
    struct ksigaction sa2 = {handler, SA_SIGINFO | SA_RESTORER | SA_ONSTACK, restore_rt, 0};
    sc6(SYS_rt_sigaction, SIGUSR2, (i64)&sa2, 0, 8, 0, 0);
    got_sig = 0;
    sc(SYS_kill, pid, SIGUSR2, 0);
    report(got_sig == SIGUSR2 && got_rsp > (u64)altstack && got_rsp <= (u64)altstack + sizeof altstack,
           "altstack");

    /* A thread with its own FS base; its exit clears and wakes the child-tid word. */
    u64 flags = 0x100 | 0x200 | 0x400 | 0x800 | 0x10000 /* VM FS FILES SIGHAND THREAD */
                | 0x80000 /* SETTLS */ | 0x200000 /* CHILD_CLEARTID */ | 0x100000 /* PARENT_SETTID */;
    volatile int ptid = 0;
    i64 tid = do_clone(flags, child_stack + sizeof child_stack, &ptid, &child_tid_word, tls_child);
    for (int i = 0; i < 1000 && child_tid_word != 0; i++) {
        i64 ts[2] = {0, 10000000};
        sc6(SYS_futex, (i64)&child_tid_word, 0 /* WAIT */, child_tid_word, (i64)ts, 0, 0);
    }
    report(tid > 0 && ptid == tid && child_ran && child_fs == (u64)tls_child && child_tid_word == 0,
           "clone-thread");
    report(rdfs() == (u64)tls_main, "fs-after-clone");

    /* Costs. */
    enum { N = 100000 };
    u64 t0 = now_ns();
    for (int i = 0; i < N; i++) sc(SYS_getpid, 0, 0, 0);
    u64 t1 = now_ns();
    for (int i = 0; i < N / 10; i++) sc(SYS_getppid, 0, 0, 0);
    u64 t2 = now_ns();
    puts1("linux-probe: bench getpid ");
    putnum((t1 - t0) / N);
    puts1(" ns/call, getppid ");
    putnum((t2 - t1) / (N / 10));
    puts1(" ns/call\n");

    puts1(failures ? "linux-probe: FAIL\n" : "linux-probe: PASS\n");
    sc(SYS_exit_group, failures, 0, 0);
}

__asm__(".text\n .globl _start\n _start:\n xor %rbp, %rbp\n and $-16, %rsp\n call probe_main\n hlt\n");
