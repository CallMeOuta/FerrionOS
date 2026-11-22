#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/random.h>
#include <sys/statvfs.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef MADV_DONTNEED_LOCKED
#define MADV_DONTNEED_LOCKED 24
#endif

enum {
    PAGE_BYTES = 4096,
    MAX_SPARSE_SIZES = 5,
    MAX_CAPTURE_BYTES = 4096,
    MAX_DRAIN_ITERS = 65536,
    TEST_TIMEOUT_MS = 15000,
    POLL_STEP_MS = 20,
    MAX_TIMED_EXEC_MS = 120000,
    MAX_PATH_BYTES = 512,
    MAX_ESCAPED_BYTES = 768,
    MUTEX_STRESS_THREADS = 4,
    MUTEX_STRESS_ITERS = 200000,
    MUTEX_ATTR_ITERS = 20000,
    COND_HANDOFF_ROUNDS = 400,
    MUTEX_BENCH_ITERS = 100000,
    FUTEX_BENCH_ITERS = 10000,
    MICRO_MPROTECT_PAIRS = 1000,
    MICRO_HOT_PAGES = 64,
    MICRO_PF_PAGES = 1024,
    MICRO_OP_ITERS = 1000,
    MICRO_STATVFS_ITERS = 100,
    FUTEX_WAIT_POLL_LIMIT = 5000,
    STALE_WAKE_POLL_MS = 1000,
};

struct mem_frames {
    uint64_t used;
    uint64_t free;
    uint64_t total;
    bool valid;
};

struct suite_case {
    const char *name;
    int expected_status;
    const char *expected_stdout;
    const char *expected_stderr;
};

static const struct suite_case SUITE_CASES[] = {
    {"delete_type_size_mismatch", -6, NULL, "fatal allocator error: sized deallocation mismatch (small)\n"},
    {"aligned_sized_delete_small", 0, NULL, NULL},
    {"aligned_sized_delete_small_min_align", 0, NULL, NULL},
    {"invalid_aligned_sized_delete_small", -6, NULL, "fatal allocator error: sized deallocation mismatch (small)\n"},
    {"aligned_sized_delete_large", 0, NULL, NULL},
    {"invalid_aligned_sized_delete_large", -6, NULL, "fatal allocator error: sized deallocation mismatch (large)\n"},
    {"invalid_free_aligned_sized_small", -6, NULL, "fatal allocator error: invalid sized deallocation alignment (small)\n"},
    {"free_sized_small", 0, NULL, NULL},
    {"free_sized_large", 0, NULL, NULL},
    {"double_free_large_delayed", -6, NULL, "fatal allocator error: invalid free\n"},
    {"double_free_large", -6, NULL, "fatal allocator error: invalid free\n"},
    {"double_free_small_delayed", -6, NULL, "fatal allocator error: double free (quarantine)\n"},
    {"double_free_small", -6, NULL, "fatal allocator error: double free (quarantine)\n"},
    {"overflow_large_1_byte", -11, NULL, NULL},
    {"overflow_large_8_byte", -11, NULL, NULL},
    {"overflow_small_1_byte", -6, NULL, "fatal allocator error: canary corrupted\n"},
    {"overflow_small_8_byte", -6, NULL, "fatal allocator error: canary corrupted\n"},
    {"invalid_free_protected", -6, NULL, "fatal allocator error: invalid free\n"},
    {"invalid_free_small_region_far", -6, NULL, "fatal allocator error: invalid free within a slab yet to be used\n"},
    {"invalid_free_small_region", -6, NULL, "fatal allocator error: double free\n"},
    {"invalid_free_unprotected", -6, NULL, "fatal allocator error: invalid free\n"},
    {"invalid_malloc_usable_size_small_quarantine", -6, NULL, "fatal allocator error: invalid malloc_usable_size (quarantine)\n"},
    {"invalid_malloc_usable_size_small", -6, NULL, "fatal allocator error: invalid malloc_usable_size\n"},
    {"read_after_free_large", -11, NULL, NULL},
    {"read_after_free_small", 0, "0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n0\n", NULL},
    {"read_zero_size", -11, NULL, NULL},
    {"string_overflow", 0, "overflow by 0 bytes\n", NULL},
    {"unaligned_free_large", -6, NULL, "fatal allocator error: invalid free\n"},
    {"unaligned_free_small", -6, NULL, "fatal allocator error: invalid unaligned free\n"},
    {"unaligned_malloc_usable_size_small", -6, NULL, "fatal allocator error: invalid unaligned malloc_usable_size\n"},
    {"uninitialized_free", -6, NULL, "fatal allocator error: invalid free\n"},
    {"uninitialized_malloc_usable_size", -6, NULL, "fatal allocator error: invalid malloc_usable_size\n"},
    {"uninitialized_realloc", -6, NULL, "fatal allocator error: invalid realloc\n"},
    {"write_after_free_large_reuse", -11, NULL, NULL},
    {"write_after_free_large", -11, NULL, NULL},
    {"write_after_free_small_reuse", -6, NULL, "fatal allocator error: detected write after free\n"},
    {"write_after_free_small", -6, NULL, "fatal allocator error: detected write after free\n"},
    {"write_zero_size", -11, NULL, NULL},
    {"malloc_object_size", 0, NULL, NULL},
    {"malloc_object_size_offset", 0, NULL, NULL},
    {"invalid_malloc_object_size_small", -6, NULL, "fatal allocator error: invalid malloc_object_size\n"},
    {"invalid_malloc_object_size_small_quarantine", -6, NULL, "fatal allocator error: invalid malloc_object_size (quarantine)\n"},
    {"invalid_malloc_object_size_small_canary", -6, NULL, "fatal allocator error: invalid malloc_object_size (canary)\n"},
    {"impossibly_large_malloc", 0, NULL, NULL},
    {"uninitialized_read_small", 0, NULL, NULL},
    {"uninitialized_read_large", 0, NULL, NULL},
    {"realloc_init", 0, NULL, NULL},
    {"large_array_growth", 0, NULL, NULL},
    {"malloc_object_size_zero", 0, NULL, NULL},
    {"calloc_overflow", 0, NULL, NULL},
    {"reallocarray_overflow", 0, NULL, NULL},
    {"calloc_zeroed", 0, NULL, NULL},
    {"pvalloc", 0, NULL, NULL},
    {"posix_memalign_einval", 0, NULL, NULL},
    {"aligned_alloc_einval", 0, NULL, NULL},
    {"aligned_alloc_zero_size", 0, NULL, NULL},
    {"malloc_zero_different", 0, NULL, NULL},
    {"malloc_noreuse", 0, NULL, NULL},
};

static uint64_t now_us(void) {
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000000ULL + (uint64_t)ts.tv_nsec / 1000ULL;
}

static struct mem_frames read_mem_frames(void) {
    struct statvfs st;
    struct mem_frames out = {0, 0, 0, false};
    if (statvfs("/scheme/memory", &st) == 0) {
        out.total = (uint64_t)st.f_blocks;
        out.free = (uint64_t)st.f_bfree;
        out.used = out.total >= out.free ? out.total - out.free : 0;
        out.valid = true;
    }
    return out;
}

static int wait_child_status(pid_t pid, int timeout_ms, bool *timed_out) {
    assert(pid > 0);
    assert(timeout_ms > 0);
    *timed_out = false;
    uint64_t deadline = now_us() + (uint64_t)timeout_ms * 1000ULL;
    for (int iter = 0; iter < MAX_DRAIN_ITERS; iter++) {
        int status = 0;
        pid_t r = waitpid(pid, &status, WNOHANG);
        if (r == pid) {
            if (WIFEXITED(status)) {
                return WEXITSTATUS(status);
            }
            if (WIFSIGNALED(status)) {
                return -WTERMSIG(status);
            }
            return -255;
        }
        if (r < 0) {
            return -254;
        }
        if (now_us() >= deadline) {
            *timed_out = true;
            kill(pid, SIGKILL);
            (void)waitpid(pid, &status, 0);
            return -9;
        }
        struct timespec req = {0, POLL_STEP_MS * 1000000L};
        nanosleep(&req, NULL);
    }
    *timed_out = true;
    kill(pid, SIGKILL);
    (void)waitpid(pid, NULL, 0);
    return -9;
}

static int run_sparse(void) {
    static const size_t SIZES[MAX_SPARSE_SIZES] = {
        32ULL * 1024ULL * 1024ULL,
        256ULL * 1024ULL * 1024ULL,
        1024ULL * 1024ULL * 1024ULL,
        4ULL * 1024ULL * 1024ULL * 1024ULL,
        32ULL * 1024ULL * 1024ULL * 1024ULL,
    };
    struct mem_frames init = read_mem_frames();
    printf("sparse-header memstat_valid=%d total_frames=%llu used_frames=%llu free_frames=%llu\n",
           init.valid ? 1 : 0,
           (unsigned long long)init.total,
           (unsigned long long)init.used,
           (unsigned long long)init.free);
    for (size_t i = 0; i < MAX_SPARSE_SIZES; i++) {
        size_t sz = SIZES[i];
        struct mem_frames f0 = read_mem_frames();
        uint64_t t0 = now_us();
        void *p = mmap(NULL, sz, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        uint64_t t1 = now_us();
        struct mem_frames f1 = read_mem_frames();
        if (p == MAP_FAILED) {
            printf("sparse size=%zu status=MMAP_FAIL errno=%d mmap_us=%llu\n",
                   sz, errno, (unsigned long long)(t1 - t0));
            continue;
        }
        char *mid = (char *)p + (sz / 2);
        uint64_t t2 = now_us();
        int mp_rw = mprotect(mid, PAGE_BYTES, PROT_READ | PROT_WRITE);
        if (mp_rw == 0) {
            volatile char *vmid = (volatile char *)mid;
            *vmid = 0x5A;
        }
        uint64_t t3 = now_us();
        struct mem_frames f2 = read_mem_frames();
        uint64_t t4 = now_us();
        int mp_none = mprotect(mid, PAGE_BYTES, PROT_NONE);
        uint64_t t5 = now_us();
        struct mem_frames f3 = read_mem_frames();
        uint64_t t6 = now_us();
        int unmap_rc = munmap(p, sz);
        uint64_t t7 = now_us();
        struct mem_frames f4 = read_mem_frames();
        int64_t d_mmap = (int64_t)f1.used - (int64_t)f0.used;
        int64_t d_touch = (int64_t)f2.used - (int64_t)f1.used;
        int64_t d_prot = (int64_t)f3.used - (int64_t)f2.used;
        int64_t d_unmap = (int64_t)f4.used - (int64_t)f3.used;
        int64_t d_net = (int64_t)f4.used - (int64_t)f0.used;
        printf("sparse size=%zu status=OK mmap_us=%llu mmap_frames=%lld mp_rw=%d touch_us=%llu touch_frames=%lld mp_none=%d prot_us=%llu prot_frames=%lld unmap_rc=%d unmap_us=%llu unmap_frames=%lld net_frames=%lld\n",
               sz,
               (unsigned long long)(t1 - t0),
               (long long)d_mmap,
               mp_rw,
               (unsigned long long)(t3 - t2),
               (long long)d_touch,
               mp_none,
               (unsigned long long)(t5 - t4),
               (long long)d_prot,
               unmap_rc,
               (unsigned long long)(t7 - t6),
               (long long)d_unmap,
               (long long)d_net);
        fflush(stdout);
    }
    return 0;
}

static void probe_prot_case(const char *label, bool via_mprotect, bool is_write) {
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        int init_prot = via_mprotect ? (PROT_READ | PROT_WRITE) : PROT_NONE;
        volatile unsigned char *p = mmap(NULL, PAGE_BYTES, init_prot, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (p == MAP_FAILED) {
            _exit(101);
        }
        if (via_mprotect) {
            p[0] = 0xA5;
            if (mprotect((void *)p, PAGE_BYTES, PROT_NONE) != 0) {
                _exit(102);
            }
        }
        if (is_write) {
            p[0] = 0x5A;
            _exit(0);
        } else {
            unsigned char v = p[0];
            _exit((int)(10 + (v == 0xA5 ? 2 : (v == 0 ? 1 : 3))));
        }
    }
    bool timed_out = false;
    int st = wait_child_status(pid, 5000, &timed_out);
    printf("prot-none case=%s status=%d timed_out=%d\n", label, st, timed_out ? 1 : 0);
}

static int run_prot_none(void) {
    probe_prot_case("mmap-none-read", false, false);
    probe_prot_case("mmap-none-write", false, true);
    probe_prot_case("mprotect-none-read", true, false);
    probe_prot_case("mprotect-none-write", true, true);
    return 0;
}

static unsigned char ro_test_page[PAGE_BYTES] __attribute__((aligned(PAGE_BYTES)));

static int run_syscalls(void) {
    unsigned char *p = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p != MAP_FAILED);
    p[0] = 0x42;
    errno = 0;
    void *mr = mremap(p, PAGE_BYTES, PAGE_BYTES * 2, MREMAP_MAYMOVE);
    int mr_errno = errno;
    printf("syscall-mremap ok=%d errno=%d\n", mr != MAP_FAILED ? 1 : 0, mr_errno);
    if (mr != MAP_FAILED) {
        munmap(mr, PAGE_BYTES * 2);
    } else {
        munmap(p, PAGE_BYTES);
    }

    size_t adv_sz = 16 * PAGE_BYTES;
    unsigned char *ap = mmap(NULL, adv_sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(ap != MAP_FAILED);
    memset(ap, 0x5A, adv_sz);
    struct mem_frames before_adv = read_mem_frames();
    errno = 0;
    int r1 = madvise(ap, adv_sz, MADV_DONTNEED);
    int e1 = errno;
    struct mem_frames after_adv1 = read_mem_frames();
    unsigned char b1 = ap[0];
    errno = 0;
    int r2 = madvise(ap, adv_sz, MADV_DONTNEED_LOCKED);
    int e2 = errno;
    struct mem_frames after_adv2 = read_mem_frames();
    unsigned char b2 = ap[0];
    printf("syscall-madvise dontneed_rc=%d dontneed_errno=%d byte_after=0x%02x frames_delta=%lld locked_rc=%d locked_errno=%d locked_byte_after=0x%02x locked_frames_delta=%lld\n",
           r1, e1, b1, (long long)((int64_t)after_adv1.used - (int64_t)before_adv.used),
           r2, e2, b2, (long long)((int64_t)after_adv2.used - (int64_t)after_adv1.used));
    munmap(ap, adv_sz);

    unsigned char rbuf1[32] = {0};
    unsigned char rbuf2[32] = {0};
    errno = 0;
    ssize_t gr1 = getrandom(rbuf1, sizeof(rbuf1), 0);
    int ge1 = errno;
    ssize_t gr2 = getrandom(rbuf2, sizeof(rbuf2), 0);
    int ge2 = errno;
    int diff = memcmp(rbuf1, rbuf2, sizeof(rbuf1)) != 0;
    printf("syscall-getrandom r1=%zd e1=%d r2=%zd e2=%d differ=%d\n", gr1, ge1, gr2, ge2, diff);

    unsigned char *pm = mmap(NULL, 16 * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(pm != MAP_FAILED);
    pm[4 * PAGE_BYTES] = 0x77;
    pm[11 * PAGE_BYTES] = 0x88;
    int u1 = munmap(pm, 4 * PAGE_BYTES);
    int u2 = munmap(pm + 12 * PAGE_BYTES, 4 * PAGE_BYTES);
    int mid_ok = (pm[4 * PAGE_BYTES] == 0x77 && pm[11 * PAGE_BYTES] == 0x88) ? 1 : 0;
    int u3 = munmap(pm + 4 * PAGE_BYTES, 8 * PAGE_BYTES);
    printf("syscall-partial-munmap u1=%d u2=%d mid_ok=%d u3=%d\n", u1, u2, mid_ok, u3);

    ro_test_page[0] = 0x33;
    int ro_rc = mprotect(ro_test_page, PAGE_BYTES, PROT_READ);
    int ro_write_status = 0;
    bool ro_to = false;
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        volatile unsigned char *vp = ro_test_page;
        *vp = 0x44;
        _exit(0);
    }
    ro_write_status = wait_child_status(pid, 5000, &ro_to);
    printf("syscall-mprotect-ro rc=%d write_status=%d timed_out=%d\n", ro_rc, ro_write_status, ro_to ? 1 : 0);
    return 0;
}

static void escape_text(const char *src, size_t len, char *dst, size_t dst_cap) {
    assert(dst_cap >= 4);
    size_t out = 0;
    for (size_t i = 0; i < len && out + 4 < dst_cap; i++) {
        unsigned char c = (unsigned char)src[i];
        if (c == '\n') {
            dst[out++] = '\\';
            dst[out++] = 'n';
        } else if (c == '\r') {
            dst[out++] = '\\';
            dst[out++] = 'r';
        } else if (c >= 0x20 && c < 0x7f) {
            dst[out++] = (char)c;
        } else {
            dst[out++] = '?';
        }
    }
    dst[out] = '\0';
}

static void drain_nonblock(int fd, char *buf, size_t *len, size_t cap) {
    char tmp[512];
    for (int i = 0; i < 64; i++) {
        ssize_t n = read(fd, tmp, sizeof(tmp));
        if (n <= 0) {
            break;
        }
        size_t avail = cap > *len ? cap - *len : 0;
        size_t copy = (size_t)n < avail ? (size_t)n : avail;
        if (copy > 0) {
            memcpy(buf + *len, tmp, copy);
            *len += copy;
        }
    }
}

static int run_one_suite_case(const char *dir, const struct suite_case *tc) {
    char path[MAX_PATH_BYTES];
    int n = snprintf(path, sizeof(path), "%s/%s", dir, tc->name);
    if (n <= 0 || (size_t)n >= sizeof(path)) {
        printf("suite %s match=FAIL reason=path_too_long\n", tc->name);
        return 0;
    }

    int out_pipe[2];
    int err_pipe[2];
    assert(pipe(out_pipe) == 0);
    assert(pipe(err_pipe) == 0);

    uint64_t t0 = now_us();
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        dup2(out_pipe[1], STDOUT_FILENO);
        dup2(err_pipe[1], STDERR_FILENO);
        close(out_pipe[0]);
        close(out_pipe[1]);
        close(err_pipe[0]);
        close(err_pipe[1]);
        char *argv_child[] = {path, NULL};
        execv(path, argv_child);
        _exit(127);
    }

    close(out_pipe[1]);
    close(err_pipe[1]);
    fcntl(out_pipe[0], F_SETFL, O_NONBLOCK);
    fcntl(err_pipe[0], F_SETFL, O_NONBLOCK);

    char out_buf[MAX_CAPTURE_BYTES + 1];
    char err_buf[MAX_CAPTURE_BYTES + 1];
    size_t out_len = 0;
    size_t err_len = 0;
    int result_status = -255;
    bool done = false;
    bool timed_out = false;
    uint64_t deadline = now_us() + (uint64_t)TEST_TIMEOUT_MS * 1000ULL;

    for (int iter = 0; iter < MAX_DRAIN_ITERS; iter++) {
        drain_nonblock(out_pipe[0], out_buf, &out_len, MAX_CAPTURE_BYTES);
        drain_nonblock(err_pipe[0], err_buf, &err_len, MAX_CAPTURE_BYTES);
        int status = 0;
        pid_t r = waitpid(pid, &status, WNOHANG);
        if (r == pid) {
            if (WIFEXITED(status)) {
                result_status = WEXITSTATUS(status);
            } else if (WIFSIGNALED(status)) {
                result_status = -WTERMSIG(status);
            }
            done = true;
            break;
        }
        if (now_us() >= deadline) {
            timed_out = true;
            kill(pid, SIGKILL);
            (void)waitpid(pid, &status, 0);
            result_status = -9;
            break;
        }
        struct timespec req = {0, POLL_STEP_MS * 1000000L};
        nanosleep(&req, NULL);
    }
    if (!done && !timed_out) {
        timed_out = true;
        kill(pid, SIGKILL);
        (void)waitpid(pid, NULL, 0);
        result_status = -9;
    }

    drain_nonblock(out_pipe[0], out_buf, &out_len, MAX_CAPTURE_BYTES);
    drain_nonblock(err_pipe[0], err_buf, &err_len, MAX_CAPTURE_BYTES);
    out_buf[out_len] = '\0';
    err_buf[err_len] = '\0';
    close(out_pipe[0]);
    close(err_pipe[0]);
    uint64_t elapsed_ms = (now_us() - t0) / 1000ULL;

    bool ok = !timed_out && (result_status == tc->expected_status);
    if (ok && tc->expected_stdout != NULL) {
        ok = (strcmp(out_buf, tc->expected_stdout) == 0);
    }
    if (ok && tc->expected_stderr != NULL) {
        ok = (strcmp(err_buf, tc->expected_stderr) == 0);
    }

    char esc_err[MAX_ESCAPED_BYTES];
    char esc_out[MAX_ESCAPED_BYTES];
    escape_text(err_buf, err_len, esc_err, sizeof(esc_err));
    escape_text(out_buf, out_len, esc_out, sizeof(esc_out));

    printf("suite %s match=%s status=%d expected=%d ms=%llu timed_out=%d stdout=\"%s\" stderr=\"%s\"\n",
           tc->name, ok ? "PASS" : "FAIL", result_status, tc->expected_status,
           (unsigned long long)elapsed_ms, timed_out ? 1 : 0, esc_out, esc_err);
    fflush(stdout);
    return ok ? 1 : 0;
}

static int run_suite(const char *dir) {
    size_t total = sizeof(SUITE_CASES) / sizeof(SUITE_CASES[0]);
    size_t passed = 0;
    for (size_t i = 0; i < total; i++) {
        passed += (size_t)run_one_suite_case(dir, &SUITE_CASES[i]);
    }
    printf("suite-summary passed=%zu failed=%zu total=%zu\n", passed, total - passed, total);
    fflush(stdout);
    return 0;
}

static int run_timed_exec(int timeout_ms, char **cmd) {
    assert(timeout_ms > 0 && timeout_ms <= MAX_TIMED_EXEC_MS);
    assert(cmd[0] != NULL);
    struct mem_frames f0 = read_mem_frames();
    uint64_t t0 = now_us();
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        execvp(cmd[0], cmd);
        _exit(127);
    }
    bool timed_out = false;
    int status = wait_child_status(pid, timeout_ms, &timed_out);
    uint64_t t1 = now_us();
    struct mem_frames f1 = read_mem_frames();
    printf("timed-exec cmd=%s status=%d timed_out=%d ms=%llu net_frames=%lld\n",
           cmd[0], status, timed_out ? 1 : 0, (unsigned long long)((t1 - t0) / 1000ULL),
           (long long)((int64_t)f1.used - (int64_t)f0.used));
    fflush(stdout);
    return 0;
}

#if defined(__x86_64__) && defined(__redox__)
extern uintptr_t redox_cur_thrfd_v0(void);
extern uintptr_t redox_dup_v1(uintptr_t fd, const char *buf, uintptr_t len);
extern uintptr_t redox_write_v1(uintptr_t fd, const void *buf, uintptr_t len);
extern uintptr_t redox_close_v1(uintptr_t fd);

__attribute__((used)) volatile uint64_t fc_futex_wake_count;
__attribute__((used)) volatile uint64_t fc_total_syscalls;
extern char hm_fc_start[], hm_fc_entry[], hm_fc_end[];

__asm__(
    ".intel_syntax noprefix\n"
    ".text\n"
    ".p2align 4\n"
    ".globl hm_fc_start\n"
    ".globl hm_fc_entry\n"
    ".globl hm_fc_end\n"
    "hm_fc_start:\n"
    "hm_fc_entry:\n"
    "    lea rsp, [rsp - 128]\n"
    "    push rcx\n"
    "    push r11\n"
    "    inc qword ptr [rip + fc_total_syscalls]\n"
    "    cmp rax, 240\n"
    "    jne 1f\n"
    "    cmp rsi, 1\n"
    "    jne 1f\n"
    "    inc qword ptr [rip + fc_futex_wake_count]\n"
    "1:\n"
    "    syscall\n"
    "    popfq\n"
    "    pop rcx\n"
    "    lea rsp, [rsp + 128]\n"
    "    jmp rcx\n"
    "hm_fc_end:\n"
    ".att_syntax prefix\n"
);

static inline long raw_sys_futex(uint32_t *addr, long op, long val, const struct timespec *ts) {
    long ret;
    register long r10 __asm__("r10") = (long)(uintptr_t)ts;
    register long r8 __asm__("r8") = 0;
    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "a"(240L), "D"(addr), "S"(op), "d"(val), "r"(r10), "r"(r8)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static long fc_open_and_enable(void) {
    fc_futex_wake_count = 0;
    fc_total_syscalls = 0;
    uintptr_t fd = redox_dup_v1(redox_cur_thrfd_v0(), "foreign-call", 12);
    if (fd > (uintptr_t)-4096) {
        return -1;
    }
    uintptr_t words[3] = {(uintptr_t)hm_fc_entry, (uintptr_t)hm_fc_start, (uintptr_t)hm_fc_end};
    uintptr_t w = redox_write_v1(fd, words, sizeof(words));
    if (w > (uintptr_t)-4096) {
        (void)redox_close_v1(fd);
        return -1;
    }
    return (long)fd;
}

static void fc_disable_and_close(long fd) {
    if (fd >= 0) {
        uintptr_t zero[3] = {0, 0, 0};
        (void)redox_write_v1((uintptr_t)fd, zero, sizeof(zero));
        (void)redox_close_v1((uintptr_t)fd);
    }
}
#endif

struct counter_ctx {
    pthread_mutex_t *mu;
    uint64_t *counter;
    int iters;
    int recursive_depth;
    int use_trylock_yield;
};

static void *counter_worker(void *arg) {
    struct counter_ctx *ctx = (struct counter_ctx *)arg;
    for (int i = 0; i < ctx->iters; i++) {
        if (ctx->use_trylock_yield && (i & 15) == 0) {
            int trc = pthread_mutex_trylock(ctx->mu);
            if (trc == EBUSY) {
                assert(pthread_mutex_lock(ctx->mu) == 0);
            } else {
                assert(trc == 0);
            }
            (*ctx->counter)++;
            assert(pthread_mutex_unlock(ctx->mu) == 0);
            if ((i & 4095) == 0) {
                sched_yield();
            }
            continue;
        }
        for (int d = 0; d < ctx->recursive_depth; d++) {
            assert(pthread_mutex_lock(ctx->mu) == 0);
        }
        (*ctx->counter)++;
        for (int d = 0; d < ctx->recursive_depth; d++) {
            assert(pthread_mutex_unlock(ctx->mu) == 0);
        }
    }
    return NULL;
}

struct cond_ctx {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int turn;
    int round;
};

struct cond_arg {
    struct cond_ctx *ctx;
    int id;
};

static void *cond_worker(void *arg) {
    struct cond_arg *a = (struct cond_arg *)arg;
    struct cond_ctx *c = a->ctx;
    for (int r = 0; r < COND_HANDOFF_ROUNDS; r++) {
        assert(pthread_mutex_lock(&c->mu) == 0);
        while (c->turn != a->id) {
            assert(pthread_cond_wait(&c->cv, &c->mu) == 0);
        }
        c->turn = (c->turn + 1) % MUTEX_STRESS_THREADS;
        c->round++;
        assert(pthread_cond_broadcast(&c->cv) == 0);
        assert(pthread_mutex_unlock(&c->mu) == 0);
    }
    return NULL;
}

struct timed_ctx {
    pthread_mutex_t mu;
    atomic_int holder_ready;
    atomic_int waiter_done;
    int timed_rc;
};

static void *timed_waiter_fn(void *arg) {
    struct timed_ctx *tc = (struct timed_ctx *)arg;
    struct timespec ts;
    assert(clock_gettime(CLOCK_REALTIME, &ts) == 0);
    ts.tv_nsec += 30000000L;
    if (ts.tv_nsec >= 1000000000L) {
        ts.tv_sec += 1;
        ts.tv_nsec -= 1000000000L;
    }
    tc->timed_rc = pthread_mutex_timedlock(&tc->mu, &ts);
    if (tc->timed_rc == 0) {
        pthread_mutex_unlock(&tc->mu);
    }
    return NULL;
}

static void *peer_waiter_fn(void *arg) {
    struct timed_ctx *tc = (struct timed_ctx *)arg;
    assert(pthread_mutex_lock(&tc->mu) == 0);
    atomic_store(&tc->waiter_done, 1);
    assert(pthread_mutex_unlock(&tc->mu) == 0);
    return NULL;
}

struct stale_timed_ctx {
    pthread_mutex_t mu;
    atomic_int waiter_entered;
    atomic_int waiter_done;
    int timed_rc;
};

static void *stale_peer_waiter_fn(void *arg) {
    struct stale_timed_ctx *sc = (struct stale_timed_ctx *)arg;
    atomic_store(&sc->waiter_entered, 1);
    assert(pthread_mutex_lock(&sc->mu) == 0);
    atomic_store(&sc->waiter_done, 1);
    assert(pthread_mutex_unlock(&sc->mu) == 0);
    return NULL;
}

static void *stale_timed_waiter_fn(void *arg) {
    struct stale_timed_ctx *sc = (struct stale_timed_ctx *)arg;
    struct timespec ts;
    assert(clock_gettime(CLOCK_REALTIME, &ts) == 0);
    ts.tv_nsec += 30000000L;
    if (ts.tv_nsec >= 1000000000L) {
        ts.tv_sec += 1;
        ts.tv_nsec -= 1000000000L;
    }
    sc->timed_rc = pthread_mutex_timedlock(&sc->mu, &ts);
    if (sc->timed_rc == 0) {
        pthread_mutex_unlock(&sc->mu);
    }
    return NULL;
}

#if defined(__x86_64__) && defined(__redox__)
struct futex_handoff_ctx {
    _Atomic uint32_t *word;
    atomic_int entered;
    atomic_int woke;
};

static void *futex_waiter_untouched_fn(void *arg) {
    struct futex_handoff_ctx *fc = (struct futex_handoff_ctx *)arg;
    atomic_store(&fc->entered, 1);
    (void)raw_sys_futex((uint32_t *)fc->word, 0, 0, NULL);
    for (int i = 0; i < 64; i++) {
        if (atomic_load(fc->word) != 0) {
            break;
        }
        (void)raw_sys_futex((uint32_t *)fc->word, 0, 0, NULL);
    }
    atomic_store(&fc->woke, 1);
    return NULL;
}

static void *futex_waiter_fn(void *arg) {
    struct futex_handoff_ctx *fc = (struct futex_handoff_ctx *)arg;
    atomic_store(&fc->entered, 1);
    for (int i = 0; i < 64; i++) {
        uint32_t cur = atomic_load(fc->word);
        if (cur != 0) {
            break;
        }
        (void)raw_sys_futex((uint32_t *)fc->word, 0, 0, NULL);
    }
    atomic_store(&fc->woke, 1);
    return NULL;
}
#endif

static int run_mutex_stress_inner(void) {
    pthread_mutex_t normal_mu = PTHREAD_MUTEX_INITIALIZER;
    uint64_t normal_count = 0;
    struct counter_ctx nctx = {&normal_mu, &normal_count, MUTEX_STRESS_ITERS, 1, 1};
    pthread_t threads[MUTEX_STRESS_THREADS];
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        assert(pthread_create(&threads[i], NULL, counter_worker, &nctx) == 0);
    }
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        assert(pthread_join(threads[i], NULL) == 0);
    }
    uint64_t expected_normal = (uint64_t)MUTEX_STRESS_THREADS * (uint64_t)MUTEX_STRESS_ITERS;
    uint64_t expected_attr = (uint64_t)MUTEX_STRESS_THREADS * (uint64_t)MUTEX_ATTR_ITERS;
    int normal_ok = (normal_count == expected_normal);

    pthread_mutexattr_t rattr;
    pthread_mutex_t rec_mu;
    assert(pthread_mutexattr_init(&rattr) == 0);
    assert(pthread_mutexattr_settype(&rattr, PTHREAD_MUTEX_RECURSIVE) == 0);
    assert(pthread_mutex_init(&rec_mu, &rattr) == 0);
    uint64_t rec_count = 0;
    struct counter_ctx rctx = {&rec_mu, &rec_count, MUTEX_ATTR_ITERS, 2, 0};
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        assert(pthread_create(&threads[i], NULL, counter_worker, &rctx) == 0);
    }
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        assert(pthread_join(threads[i], NULL) == 0);
    }
    int rec_ok = (rec_count == expected_attr);
    pthread_mutex_destroy(&rec_mu);
    pthread_mutexattr_destroy(&rattr);

    pthread_mutexattr_t eattr;
    pthread_mutex_t err_mu;
    assert(pthread_mutexattr_init(&eattr) == 0);
    assert(pthread_mutexattr_settype(&eattr, PTHREAD_MUTEX_ERRORCHECK) == 0);
    assert(pthread_mutex_init(&err_mu, &eattr) == 0);
    assert(pthread_mutex_lock(&err_mu) == 0);
    int relock_rc = pthread_mutex_lock(&err_mu);
    assert(pthread_mutex_unlock(&err_mu) == 0);
    int unowned_rc = pthread_mutex_unlock(&err_mu);
    uint64_t err_count = 0;
    struct counter_ctx ectx = {&err_mu, &err_count, MUTEX_ATTR_ITERS, 1, 0};
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        assert(pthread_create(&threads[i], NULL, counter_worker, &ectx) == 0);
    }
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        assert(pthread_join(threads[i], NULL) == 0);
    }
    int errck_ok = (relock_rc != 0 && unowned_rc == EPERM && err_count == expected_attr);
    pthread_mutex_destroy(&err_mu);
    pthread_mutexattr_destroy(&eattr);

    struct cond_ctx cctx;
    assert(pthread_mutex_init(&cctx.mu, NULL) == 0);
    assert(pthread_cond_init(&cctx.cv, NULL) == 0);
    cctx.turn = 0;
    cctx.round = 0;
    struct cond_arg cargs[MUTEX_STRESS_THREADS];
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        cargs[i].ctx = &cctx;
        cargs[i].id = i;
        assert(pthread_create(&threads[i], NULL, cond_worker, &cargs[i]) == 0);
    }
    for (int i = 0; i < MUTEX_STRESS_THREADS; i++) {
        assert(pthread_join(threads[i], NULL) == 0);
    }
    int cond_ok = (cctx.round == MUTEX_STRESS_THREADS * COND_HANDOFF_ROUNDS);
    pthread_cond_destroy(&cctx.cv);
    pthread_mutex_destroy(&cctx.mu);

    struct timed_ctx tctx;
    assert(pthread_mutex_init(&tctx.mu, NULL) == 0);
    atomic_init(&tctx.holder_ready, 0);
    atomic_init(&tctx.waiter_done, 0);
    tctx.timed_rc = -1;
    assert(pthread_mutex_lock(&tctx.mu) == 0);
    pthread_t t_timed, t_peer;
    assert(pthread_create(&t_timed, NULL, timed_waiter_fn, &tctx) == 0);
    assert(pthread_create(&t_peer, NULL, peer_waiter_fn, &tctx) == 0);
    struct timespec hold_ts = {0, 120000000L};
    nanosleep(&hold_ts, NULL);
    assert(pthread_join(t_timed, NULL) == 0);
    assert(pthread_mutex_unlock(&tctx.mu) == 0);
    assert(pthread_join(t_peer, NULL) == 0);
    int timed_ok = (tctx.timed_rc == ETIMEDOUT && atomic_load(&tctx.waiter_done) == 1);
    pthread_mutex_destroy(&tctx.mu);

    struct stale_timed_ctx sctx;
    assert(pthread_mutex_init(&sctx.mu, NULL) == 0);
    atomic_init(&sctx.waiter_entered, 0);
    atomic_init(&sctx.waiter_done, 0);
    sctx.timed_rc = -1;
    assert(pthread_mutex_lock(&sctx.mu) == 0);
    pthread_t t_sw, t_st;
    assert(pthread_create(&t_sw, NULL, stale_peer_waiter_fn, &sctx) == 0);
    for (int i = 0; i < FUTEX_WAIT_POLL_LIMIT && atomic_load(&sctx.waiter_entered) == 0; i++) {
        struct timespec st = {0, 1000000L};
        nanosleep(&st, NULL);
    }
    struct timespec sw_settle = {0, 20000000L};
    nanosleep(&sw_settle, NULL);
    assert(pthread_create(&t_st, NULL, stale_timed_waiter_fn, &sctx) == 0);
    assert(pthread_join(t_st, NULL) == 0);
    struct timespec sw_requeue = {0, 50000000L};
    nanosleep(&sw_requeue, NULL);
    assert(pthread_mutex_unlock(&sctx.mu) == 0);
    for (int i = 0; i < STALE_WAKE_POLL_MS && atomic_load(&sctx.waiter_done) == 0; i++) {
        struct timespec st = {0, 1000000L};
        nanosleep(&st, NULL);
    }
    int stale_timed_ok = (sctx.timed_rc == ETIMEDOUT && atomic_load(&sctx.waiter_done) == 1);
    if (stale_timed_ok) {
        assert(pthread_join(t_sw, NULL) == 0);
        pthread_mutex_destroy(&sctx.mu);
    }

    int futex_key_ok = 1;
#if defined(__x86_64__) && defined(__redox__)
    void *fpage = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(fpage != MAP_FAILED);
    _Atomic uint32_t *fword = (_Atomic uint32_t *)fpage;
    struct futex_handoff_ctx fh = {fword, ATOMIC_VAR_INIT(0), ATOMIC_VAR_INIT(0)};
    pthread_t t_fw;
    assert(pthread_create(&t_fw, NULL, futex_waiter_fn, &fh) == 0);
    for (int i = 0; i < FUTEX_WAIT_POLL_LIMIT && atomic_load(&fh.entered) == 0; i++) {
        struct timespec st = {0, 1000000L};
        nanosleep(&st, NULL);
    }
    struct timespec settle = {0, 20000000L};
    nanosleep(&settle, NULL);
    atomic_store(fword, 1);
    long woken = raw_sys_futex((uint32_t *)fword, 1, 1, NULL);
    assert(pthread_join(t_fw, NULL) == 0);

    void *untouched = mmap(NULL, 64ULL * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(untouched != MAP_FAILED);
    _Atomic uint32_t *uword = (_Atomic uint32_t *)((char *)untouched + 32ULL * PAGE_BYTES);
    long untouched_empty_rc = raw_sys_futex((uint32_t *)((char *)untouched + 48ULL * PAGE_BYTES), 1, 1, NULL);
    struct futex_handoff_ctx ufh = {uword, ATOMIC_VAR_INIT(0), ATOMIC_VAR_INIT(0)};
    pthread_t t_ufw;
    assert(pthread_create(&t_ufw, NULL, futex_waiter_untouched_fn, &ufh) == 0);
    for (int i = 0; i < FUTEX_WAIT_POLL_LIMIT && atomic_load(&ufh.entered) == 0; i++) {
        struct timespec st = {0, 1000000L};
        nanosleep(&st, NULL);
    }
    nanosleep(&settle, NULL);
    atomic_store(uword, 1);
    long uwoken = raw_sys_futex((uint32_t *)uword, 1, 1, NULL);
    assert(pthread_join(t_ufw, NULL) == 0);

    void *none_page = mmap(NULL, PAGE_BYTES, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(none_page != MAP_FAILED);
    long none_rc = raw_sys_futex((uint32_t *)none_page, 1, 1, NULL);
    void *mprot_none = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(mprot_none != MAP_FAILED);
    *(volatile uint32_t *)mprot_none = 7;
    assert(mprotect(mprot_none, PAGE_BYTES, PROT_NONE) == 0);
    long mprot_none_rc = raw_sys_futex((uint32_t *)mprot_none, 1, 1, NULL);
    long kern_rc = raw_sys_futex((uint32_t *)(uintptr_t)0xffffffff80000000ULL, 1, 1, NULL);
    munmap(mprot_none, PAGE_BYTES);
    munmap(none_page, PAGE_BYTES);
    munmap(untouched, 64ULL * PAGE_BYTES);
    munmap(fpage, PAGE_BYTES);
    futex_key_ok = (woken >= 0 && atomic_load(&fh.woke) == 1 && uwoken >= 0 && atomic_load(&ufh.woke) == 1 && untouched_empty_rc == 0 && none_rc == -EFAULT && mprot_none_rc == -EFAULT && kern_rc == -EFAULT);
#endif

    int all_ok = normal_ok && rec_ok && errck_ok && cond_ok && timed_ok && stale_timed_ok && futex_key_ok;
    printf("mutex-stress normal=%d rec=%d errck=%d cond=%d timed=%d stale_timed=%d futex_key=%d ok=%d\n",
           normal_ok, rec_ok, errck_ok, cond_ok, timed_ok, stale_timed_ok, futex_key_ok, all_ok);
    fflush(stdout);
    return all_ok ? 0 : 1;
}

static int run_mutex_stress(void) {
    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        _exit(run_mutex_stress_inner());
    }
    bool timed_out = false;
    int st = wait_child_status(pid, TEST_TIMEOUT_MS, &timed_out);
    if (timed_out) {
        printf("mutex-stress timed_out=1 ok=0\n");
        fflush(stdout);
        return 1;
    }
    return st == 0 ? 0 : 1;
}

static int run_mutex_bench(void) {
    pthread_mutex_t m_static = PTHREAD_MUTEX_INITIALIZER;
    pthread_mutex_t m_init;
    assert(pthread_mutex_init(&m_init, NULL) == 0);

    uint64_t t0 = now_us();
    for (int i = 0; i < MUTEX_BENCH_ITERS; i++) {
        pthread_mutex_lock(&m_static);
        pthread_mutex_unlock(&m_static);
    }
    uint64_t t1 = now_us();
    uint64_t static_ns = ((t1 - t0) * 1000ULL) / MUTEX_BENCH_ITERS;

    t0 = now_us();
    for (int i = 0; i < MUTEX_BENCH_ITERS; i++) {
        pthread_mutex_lock(&m_init);
        pthread_mutex_unlock(&m_init);
    }
    t1 = now_us();
    uint64_t init_ns = ((t1 - t0) * 1000ULL) / MUTEX_BENCH_ITERS;

    int fc_enabled = 0;
    uint64_t wake_calls = 0;
    uint64_t total_sc = 0;
    uint64_t futex_wake_ns = 0;
#if defined(__x86_64__) && defined(__redox__)
    uint32_t ctrl_word = 0;
    long fcfd = fc_open_and_enable();
    if (fcfd >= 0) {
        fc_enabled = 1;
        assert(raw_sys_futex(&ctrl_word, 1, 1, NULL) >= 0);
        for (int i = 0; i < 10000; i++) {
            pthread_mutex_lock(&m_static);
            pthread_mutex_unlock(&m_static);
        }
        fc_disable_and_close(fcfd);
        wake_calls = fc_futex_wake_count;
        total_sc = fc_total_syscalls;
    }
    uint32_t fw = 0;
    t0 = now_us();
    for (int i = 0; i < FUTEX_BENCH_ITERS; i++) {
        assert(raw_sys_futex(&fw, 1, 1, NULL) >= 0);
    }
    t1 = now_us();
    futex_wake_ns = ((t1 - t0) * 1000ULL) / FUTEX_BENCH_ITERS;
#endif

    pthread_mutex_destroy(&m_init);
    printf("mutex-bench fc_enabled=%d static_ns=%llu init_ns=%llu wake_calls_10k=%llu total_sc_10k=%llu futex_wake_ns=%llu\n",
           fc_enabled,
           (unsigned long long)static_ns,
           (unsigned long long)init_ns,
           (unsigned long long)wake_calls,
           (unsigned long long)total_sc,
           (unsigned long long)futex_wake_ns);
    fflush(stdout);
    return 0;
}

static int run_micro(void) {
    pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
    uint64_t t0 = now_us();
    for (int i = 0; i < MUTEX_BENCH_ITERS; i++) {
        pthread_mutex_lock(&m);
        pthread_mutex_unlock(&m);
    }
    uint64_t t1 = now_us();
    printf("micro mutex_lock_unlock ns=%llu\n",
           (unsigned long long)(((t1 - t0) * 1000ULL) / MUTEX_BENCH_ITERS));

#if defined(__x86_64__) && defined(__redox__)
    uint32_t fw = 0;
    t0 = now_us();
    for (int i = 0; i < FUTEX_BENCH_ITERS; i++) {
        assert(raw_sys_futex(&fw, 1, 1, NULL) >= 0);
    }
    t1 = now_us();
    printf("micro futex_wake_empty ns=%llu\n",
           (unsigned long long)(((t1 - t0) * 1000ULL) / FUTEX_BENCH_ITERS));
#endif

    void *hot = mmap(NULL, (size_t)MICRO_HOT_PAGES * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(hot != MAP_FAILED);
    memset(hot, 0x33, (size_t)MICRO_HOT_PAGES * PAGE_BYTES);
    volatile unsigned char *vhot = (volatile unsigned char *)hot;
    volatile uint64_t sink = 0;

    static const size_t PAGES_LIST[3] = {1, 8, 32};
    void *r1 = mmap(NULL, 64ULL * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(r1 != MAP_FAILED);
    memset(r1, 1, 64ULL * PAGE_BYTES);
    for (int pi = 0; pi < 3; pi++) {
        size_t np = PAGES_LIST[pi];
        size_t sz = np * PAGE_BYTES;
        t0 = now_us();
        for (int i = 0; i < MICRO_MPROTECT_PAIRS; i++) {
            assert(mprotect(r1, sz, PROT_NONE) == 0);
            assert(mprotect(r1, sz, PROT_READ | PROT_WRITE) == 0);
        }
        t1 = now_us();
        printf("micro mprotect_%zup_alone ns=%llu\n",
               np, (unsigned long long)(((t1 - t0) * 1000ULL) / (2ULL * MICRO_MPROTECT_PAIRS)));

        t0 = now_us();
        for (int i = 0; i < MICRO_MPROTECT_PAIRS; i++) {
            assert(mprotect(r1, sz, PROT_NONE) == 0);
            uint64_t acc = 0;
            for (int hp = 0; hp < MICRO_HOT_PAGES; hp++) {
                acc += vhot[(size_t)hp * PAGE_BYTES];
            }
            assert(mprotect(r1, sz, PROT_READ | PROT_WRITE) == 0);
            for (int hp = 0; hp < MICRO_HOT_PAGES; hp++) {
                acc += vhot[(size_t)hp * PAGE_BYTES];
            }
            sink += acc;
        }
        t1 = now_us();
        printf("micro mprotect_%zup_64hot ns=%llu\n",
               np, (unsigned long long)(((t1 - t0) * 1000ULL) / (2ULL * MICRO_MPROTECT_PAIRS)));
    }
    munmap(hot, (size_t)MICRO_HOT_PAGES * PAGE_BYTES);
    munmap(r1, 64ULL * PAGE_BYTES);

    size_t pf_sz = (size_t)MICRO_PF_PAGES * PAGE_BYTES;
    volatile unsigned char *pf = mmap(NULL, pf_sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(pf != MAP_FAILED);
    t0 = now_us();
    for (size_t off = 0; off < pf_sz; off += PAGE_BYTES) {
        pf[off] = 1;
    }
    t1 = now_us();
    printf("micro fresh_page_fault ns=%llu\n",
           (unsigned long long)(((t1 - t0) * 1000ULL) / MICRO_PF_PAGES));
    munmap((void *)pf, pf_sz);

    t0 = now_us();
    for (int i = 0; i < MICRO_OP_ITERS; i++) {
        void *p = mmap(NULL, 4ULL * PAGE_BYTES, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        assert(p != MAP_FAILED);
        assert(munmap(p, 4ULL * PAGE_BYTES) == 0);
    }
    t1 = now_us();
    printf("micro mmap_munmap_4p_none ns=%llu\n",
           (unsigned long long)(((t1 - t0) * 1000ULL) / MICRO_OP_ITERS));

    t0 = now_us();
    for (int i = 0; i < MICRO_OP_ITERS; i++) {
        volatile unsigned char *p = mmap(NULL, 4ULL * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        assert(p != MAP_FAILED);
        for (size_t k = 0; k < 4; k++) {
            p[k * PAGE_BYTES] = (unsigned char)i;
        }
        assert(munmap((void *)p, 4ULL * PAGE_BYTES) == 0);
    }
    t1 = now_us();
    printf("micro mmap_touch_munmap_4p_rw ns=%llu\n",
           (unsigned long long)(((t1 - t0) * 1000ULL) / MICRO_OP_ITERS));

    int nullfd = open("/dev/null", O_WRONLY);
    if (nullfd >= 0) {
        char b = 'x';
        t0 = now_us();
        for (int i = 0; i < MICRO_OP_ITERS; i++) {
            assert(write(nullfd, &b, 1) == 1);
        }
        t1 = now_us();
        printf("micro write_1b_devnull ns=%llu\n",
               (unsigned long long)(((t1 - t0) * 1000ULL) / MICRO_OP_ITERS));
        close(nullfd);
    }

    unsigned char rnd[40];
    t0 = now_us();
    for (int i = 0; i < MICRO_OP_ITERS; i++) {
        assert(getrandom(rnd, sizeof(rnd), 0) == (ssize_t)sizeof(rnd));
    }
    t1 = now_us();
    printf("micro getrandom_40b ns=%llu\n",
           (unsigned long long)(((t1 - t0) * 1000ULL) / MICRO_OP_ITERS));

    struct statvfs st;
    t0 = now_us();
    for (int i = 0; i < MICRO_STATVFS_ITERS; i++) {
        (void)statvfs("/scheme/memory", &st);
    }
    t1 = now_us();
    printf("micro statvfs_memory ns=%llu\n",
           (unsigned long long)(((t1 - t0) * 1000ULL) / MICRO_STATVFS_ITERS));

    (void)sink;
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: hardened-malloc-probe <sparse|prot-none|syscalls|mutex-stress|mutex-bench|micro|run-suite DIR|timed-exec MS CMD...>\n");
        return 2;
    }
    if (strcmp(argv[1], "sparse") == 0) {
        return run_sparse();
    }
    if (strcmp(argv[1], "prot-none") == 0) {
        return run_prot_none();
    }
    if (strcmp(argv[1], "syscalls") == 0) {
        return run_syscalls();
    }
    if (strcmp(argv[1], "mutex-stress") == 0) {
        return run_mutex_stress();
    }
    if (strcmp(argv[1], "mutex-bench") == 0) {
        return run_mutex_bench();
    }
    if (strcmp(argv[1], "micro") == 0) {
        return run_micro();
    }
    if (strcmp(argv[1], "run-suite") == 0 && argc == 3) {
        return run_suite(argv[2]);
    }
    if (strcmp(argv[1], "timed-exec") == 0 && argc >= 4) {
        int ms = atoi(argv[2]);
        if (ms <= 0 || ms > MAX_TIMED_EXEC_MS) {
            fprintf(stderr, "invalid timeout\n");
            return 2;
        }
        return run_timed_exec(ms, &argv[3]);
    }
    fprintf(stderr, "unknown command: %s\n", argv[1]);
    return 2;
}
