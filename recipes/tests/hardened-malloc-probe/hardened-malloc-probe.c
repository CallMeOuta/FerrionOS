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
    MAX_SYS_READ_BYTES = 262144,
    MAX_SYS_READ_ITERS = 4096,
    RECLAIM_GIB_PAGES = 32,
    RECLAIM_RUN_PAGES = 600,
    RECLAIM_PARTIAL_LEFT = 100,
    RECLAIM_PARTIAL_MID = 400,
    MT_WORKERS = 4,
    MT_DURATION_MS = 2500,
    MT_MAX_ITERS = 200000,
    DISCARD_PAGES = 64,
    SPARSE_DISCARD_BYTES = 1024 * 1024 * 1024,
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

static int read_sys_file(const char *path, char *buf, size_t cap, size_t *out_len) {
    assert(path != NULL);
    assert(buf != NULL);
    assert(cap > 1);
    assert(out_len != NULL);
    *out_len = 0;
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    size_t total = 0;
    for (int iter = 0; iter < MAX_SYS_READ_ITERS; iter++) {
        size_t avail = (cap - 1) - total;
        if (avail == 0) {
            char discard[256];
            ssize_t n = read(fd, discard, sizeof(discard));
            if (n <= 0) {
                break;
            }
            continue;
        }
        ssize_t n = read(fd, buf + total, avail);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            close(fd);
            return -1;
        }
        if (n == 0) {
            break;
        }
        total += (size_t)n;
    }
    close(fd);
    buf[total] = '\0';
    *out_len = total;
    return 0;
}

static bool parse_context_private_kib(const char *text, pid_t target_pid, uint64_t *out_kib) {
    assert(text != NULL);
    assert(out_kib != NULL);
    const char *p = text;
    for (int line = 0; line < MAX_SYS_READ_ITERS && *p != '\0'; line++) {
        const char *nl = strchr(p, '\n');
        size_t len = nl != NULL ? (size_t)(nl - p) : strlen(p);
        if (len > 0 && len < 512) {
            char row[512];
            memcpy(row, p, len);
            row[len] = '\0';
            unsigned long pid_val = 0;
            char c1[32], c2[32], c3[32], c4[32], c5[32], c6[32], c7[32];
            double priv_num = 0.0;
            char priv_unit[16];
            if (sscanf(row, "%lu %31s %31s %31s %31s %31s %31s %31s %lf %15s",
                       &pid_val, c1, c2, c3, c4, c5, c6, c7, &priv_num, priv_unit) == 10) {
                if ((pid_t)pid_val == target_pid) {
                    if (strcmp(priv_unit, "B") == 0) {
                        *out_kib = (uint64_t)((priv_num + 1023.0) / 1024.0);
                        return true;
                    }
                    if (strcmp(priv_unit, "KB") == 0) {
                        *out_kib = (uint64_t)priv_num;
                        return true;
                    }
                    if (strcmp(priv_unit, "MB") == 0) {
                        *out_kib = (uint64_t)(priv_num * 1024.0);
                        return true;
                    }
                    if (strcmp(priv_unit, "GB") == 0) {
                        *out_kib = (uint64_t)(priv_num * 1024.0 * 1024.0);
                        return true;
                    }
                }
            }
        }
        if (nl == NULL) {
            break;
        }
        p = nl + 1;
    }
    return false;
}

static int run_sparse_ops(void) {
    const size_t sz_32g = 32ULL * 1024ULL * 1024ULL * 1024ULL;
    const size_t sz_1g = 1024ULL * 1024ULL * 1024ULL;
    const size_t boundary_off = (2ULL * 1024ULL * 1024ULL) - (64ULL * 1024ULL);

    struct mem_frames f0 = read_mem_frames();
    void *p_full = mmap(NULL, sz_32g, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p_full != MAP_FAILED);
    struct mem_frames f1 = read_mem_frames();
    for (size_t g = 0; g < RECLAIM_GIB_PAGES; g++) {
        char *pg = (char *)p_full + g * sz_1g;
        int rc = mprotect(pg, PAGE_BYTES, PROT_READ | PROT_WRITE);
        assert(rc == 0);
        pg[0] = (char)(0x20 + g);
    }
    char *run_full = (char *)p_full + (16ULL * sz_1g) + boundary_off;
    assert(mprotect(run_full, RECLAIM_RUN_PAGES * PAGE_BYTES, PROT_READ | PROT_WRITE) == 0);
    for (size_t k = 0; k < RECLAIM_RUN_PAGES; k++) {
        run_full[k * PAGE_BYTES] = (char)((k + 17) & 0xff);
    }
    struct mem_frames f2 = read_mem_frames();
    uint64_t tf0 = now_us();
    int unmap_full_rc = munmap(p_full, sz_32g);
    uint64_t tf1 = now_us();
    struct mem_frames f3 = read_mem_frames();
    printf("table-reclaim-full mmap_frames=%lld touched_frames=%lld unmap_rc=%d unmap_us=%llu net_frames=%lld\n",
           (long long)((int64_t)f1.used - (int64_t)f0.used),
           (long long)((int64_t)f2.used - (int64_t)f0.used),
           unmap_full_rc,
           (unsigned long long)(tf1 - tf0),
           (long long)((int64_t)f3.used - (int64_t)f0.used));

    struct mem_frames fp0 = read_mem_frames();
    void *p_part = mmap(NULL, sz_32g, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p_part != MAP_FAILED);
    char *run_part = (char *)p_part + (8ULL * sz_1g) + boundary_off;
    assert(mprotect(run_part, RECLAIM_RUN_PAGES * PAGE_BYTES, PROT_READ | PROT_WRITE) == 0);
    for (size_t k = 0; k < RECLAIM_RUN_PAGES; k++) {
        unsigned char pat = (unsigned char)((k * 37 + 11) & 0xff);
        run_part[k * PAGE_BYTES] = (char)pat;
        run_part[k * PAGE_BYTES + PAGE_BYTES - 1] = (char)(pat ^ 0x5A);
    }
    int mid_unmap_rc = munmap(run_part + RECLAIM_PARTIAL_LEFT * PAGE_BYTES, RECLAIM_PARTIAL_MID * PAGE_BYTES);
    int left_ok = 1;
    for (size_t k = 0; k < RECLAIM_PARTIAL_LEFT; k++) {
        unsigned char pat = (unsigned char)((k * 37 + 11) & 0xff);
        if ((unsigned char)run_part[k * PAGE_BYTES] != pat ||
            (unsigned char)run_part[k * PAGE_BYTES + PAGE_BYTES - 1] != (unsigned char)(pat ^ 0x5A)) {
            left_ok = 0;
            break;
        }
    }
    int right_ok = 1;
    for (size_t k = RECLAIM_PARTIAL_LEFT + RECLAIM_PARTIAL_MID; k < RECLAIM_RUN_PAGES; k++) {
        unsigned char pat = (unsigned char)((k * 37 + 11) & 0xff);
        if ((unsigned char)run_part[k * PAGE_BYTES] != pat ||
            (unsigned char)run_part[k * PAGE_BYTES + PAGE_BYTES - 1] != (unsigned char)(pat ^ 0x5A)) {
            right_ok = 0;
            break;
        }
    }
    size_t left_len = (size_t)((run_part + RECLAIM_PARTIAL_LEFT * PAGE_BYTES) - (char *)p_part);
    char *right_start = run_part + (RECLAIM_PARTIAL_LEFT + RECLAIM_PARTIAL_MID) * PAGE_BYTES;
    size_t right_len = (size_t)(((char *)p_part + sz_32g) - right_start);
    int u_left = munmap(p_part, left_len);
    int u_right = munmap(right_start, right_len);
    struct mem_frames fp1 = read_mem_frames();
    printf("table-reclaim-partial mid_rc=%d left_ok=%d right_ok=%d u_left=%d u_right=%d net_frames=%lld\n",
           mid_unmap_rc,
           left_ok,
           right_ok,
           u_left,
           u_right,
           (long long)((int64_t)fp1.used - (int64_t)fp0.used));

    struct mem_frames fro0 = read_mem_frames();
    void *p_ro = mmap(NULL, sz_32g, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p_ro != MAP_FAILED);
    unsigned char *ro_pg = (unsigned char *)p_ro + (4ULL * sz_1g);
    assert(mprotect(ro_pg, 4 * PAGE_BYTES, PROT_READ | PROT_WRITE) == 0);
    ro_pg[0] = 0xAB;
    int ro_mp_rc = mprotect(ro_pg, 4 * PAGE_BYTES, PROT_READ);
    pid_t ro_pid = fork();
    assert(ro_pid >= 0);
    if (ro_pid == 0) {
        volatile unsigned char *vp = ro_pg;
        *vp = 0xCD;
        _exit(0);
    }
    bool ro_to = false;
    int ro_st = wait_child_status(ro_pid, 5000, &ro_to);
    int ro_val_ok = (ro_pg[0] == 0xAB) ? 1 : 0;
    int ro_unmap_rc = munmap(p_ro, sz_32g);
    struct mem_frames fro1 = read_mem_frames();
    printf("mprotect-ro-sparse mp_rc=%d child_status=%d timed_out=%d parent_ok=%d unmap_rc=%d net_frames=%lld\n",
           ro_mp_rc,
           ro_st,
           ro_to ? 1 : 0,
           ro_val_ok,
           ro_unmap_rc,
           (long long)((int64_t)fro1.used - (int64_t)fro0.used));

    uint64_t tbf0 = now_us();
    pid_t bfk_pid = fork();
    assert(bfk_pid >= 0);
    if (bfk_pid == 0) {
        _exit(0);
    }
    bool bfk_to = false;
    int bfk_st = wait_child_status(bfk_pid, 15000, &bfk_to);
    uint64_t tbf1 = now_us();
    printf("fork-baseline status=%d timed_out=%d fork_us=%llu\n",
           bfk_st,
           bfk_to ? 1 : 0,
           (unsigned long long)(tbf1 - tbf0));

    const size_t sz_12t = 12544ULL * sz_1g;
    static char sys_buf[MAX_SYS_READ_BYTES];
    struct mem_frames f12_0 = read_mem_frames();
    uint64_t t12_m0 = now_us();
    void *p_12t = mmap(NULL, sz_12t, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    uint64_t t12_m1 = now_us();
    assert(p_12t != MAP_FAILED);
    struct mem_frames f12_1 = read_mem_frames();
    unsigned char *slab = (unsigned char *)p_12t + (6000ULL * sz_1g);
    assert(mprotect(slab, 4 * PAGE_BYTES, PROT_READ | PROT_WRITE) == 0);
    slab[0] = 0xC3;
    slab[3 * PAGE_BYTES] = 0x55;

    size_t memstat_len = 0;
    uint64_t tms0 = now_us();
    int ms_rc = read_sys_file("/scheme/sys/memstat", sys_buf, sizeof(sys_buf), &memstat_len);
    uint64_t tms1 = now_us();

    size_t ctx_len = 0;
    uint64_t tcx0 = now_us();
    int cx_rc = read_sys_file("/scheme/sys/context", sys_buf, sizeof(sys_buf), &ctx_len);
    uint64_t tcx1 = now_us();
    uint64_t priv_kib = 0;
    bool priv_found = (cx_rc == 0) && parse_context_private_kib(sys_buf, getpid(), &priv_kib);

    uint64_t tmp_s0 = now_us();
    int mp_s1 = mprotect(slab, 4 * PAGE_BYTES, PROT_NONE);
    int mp_s2 = mprotect(slab, 4 * PAGE_BYTES, PROT_READ | PROT_WRITE);
    uint64_t tmp_s1 = now_us();

    uint64_t tmp_b0 = now_us();
    int mp_b1 = mprotect((char *)p_12t + (6000ULL * sz_1g), sz_32g, PROT_READ);
    int mp_b2 = mprotect((char *)p_12t + (6000ULL * sz_1g), sz_32g, PROT_NONE);
    int mp_b3 = mprotect(slab, 4 * PAGE_BYTES, PROT_READ | PROT_WRITE);
    uint64_t tmp_b1 = now_us();

    uint64_t tfk0 = now_us();
    pid_t fk_pid = fork();
    assert(fk_pid >= 0);
    if (fk_pid == 0) {
        if (slab[0] != 0xC3 || slab[3 * PAGE_BYTES] != 0x55) {
            _exit(21);
        }
        slab[0] = 0x7E;
        if (slab[0] != 0x7E) {
            _exit(22);
        }
        _exit(0);
    }
    bool fk_to = false;
    int fk_st = wait_child_status(fk_pid, 15000, &fk_to);
    uint64_t tfk1 = now_us();
    int cow_ok = (fk_st == 0 && !fk_to && slab[0] == 0xC3 && slab[3 * PAGE_BYTES] == 0x55) ? 1 : 0;

    uint64_t tu12_0 = now_us();
    int u12_rc = munmap(p_12t, sz_12t);
    uint64_t tu12_1 = now_us();
    struct mem_frames f12_2 = read_mem_frames();

    printf("sparse-12t mmap_us=%llu mmap_frames=%lld memstat_rc=%d memstat_bytes=%zu memstat_us=%llu context_rc=%d priv_found=%d priv_kib=%llu context_us=%llu mp_slab_rc=%d mp_slab_us=%llu mp_32g_rc=%d mp_32g_us=%llu fork_status=%d cow_ok=%d fork_us=%llu unmap_rc=%d unmap_us=%llu net_frames=%lld\n",
           (unsigned long long)(t12_m1 - t12_m0),
           (long long)((int64_t)f12_1.used - (int64_t)f12_0.used),
           ms_rc,
           memstat_len,
           (unsigned long long)(tms1 - tms0),
           cx_rc,
           priv_found ? 1 : 0,
           (unsigned long long)priv_kib,
           (unsigned long long)(tcx1 - tcx0),
           (mp_s1 == 0 && mp_s2 == 0) ? 0 : 1,
           (unsigned long long)(tmp_s1 - tmp_s0),
           (mp_b1 == 0 && mp_b2 == 0 && mp_b3 == 0) ? 0 : 1,
           (unsigned long long)(tmp_b1 - tmp_b0),
           fk_st,
           cow_ok,
           (unsigned long long)(tfk1 - tfk0),
           u12_rc,
           (unsigned long long)(tu12_1 - tu12_0),
           (long long)((int64_t)f12_2.used - (int64_t)f12_0.used));
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

static int scan_log_max_early_table_free(uint64_t *out_max, size_t *out_bytes) {
    assert(out_max != NULL);
    assert(out_bytes != NULL);
    *out_max = 0;
    *out_bytes = 0;
    int fd = open("/scheme/sys/log", O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    static const char NEEDLE[] = "early-table-free count=";
    const size_t nlen = sizeof(NEEDLE) - 1;
    const size_t keep = 64;
    char buf[4096 + 64 + 1];
    size_t carry = 0;
    size_t bytes_read = 0;
    uint64_t max_val = 0;
    for (int iter = 0; iter < MAX_SYS_READ_ITERS; iter++) {
        ssize_t n = read(fd, buf + carry, 4096);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            close(fd);
            return -1;
        }
        if (n == 0) {
            break;
        }
        bytes_read += (size_t)n;
        size_t total = carry + (size_t)n;
        buf[total] = '\0';
        const char *p = buf;
        for (size_t s = 0; s < total; s++) {
            const char *hit = strstr(p, NEEDLE);
            if (hit == NULL) {
                break;
            }
            unsigned long long val = strtoull(hit + nlen, NULL, 10);
            if ((uint64_t)val > max_val) {
                max_val = (uint64_t)val;
            }
            p = hit + nlen;
        }
        if (total > keep) {
            memmove(buf, buf + (total - keep), keep);
            carry = keep;
        } else {
            carry = total;
        }
    }
    close(fd);
    *out_max = max_val;
    *out_bytes = bytes_read;
    return 0;
}

static int run_pt_order(void) {
    uint64_t before = 0;
    size_t bytes_before = 0;
    int rc0 = scan_log_max_early_table_free(&before, &bytes_before);

    const size_t sz_32g = 32ULL * 1024ULL * 1024ULL * 1024ULL;
    const size_t sz_1g = 1024ULL * 1024ULL * 1024ULL;
    const size_t boundary_off = (2ULL * 1024ULL * 1024ULL) - (64ULL * 1024ULL);
    void *p = mmap(NULL, sz_32g, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p != MAP_FAILED);
    for (size_t g = 0; g < RECLAIM_GIB_PAGES; g++) {
        char *pg = (char *)p + g * sz_1g;
        assert(mprotect(pg, PAGE_BYTES, PROT_READ | PROT_WRITE) == 0);
        pg[0] = (char)(0x30 + g);
    }
    char *run = (char *)p + (16ULL * sz_1g) + boundary_off;
    assert(mprotect(run, RECLAIM_RUN_PAGES * PAGE_BYTES, PROT_READ | PROT_WRITE) == 0);
    for (size_t k = 0; k < RECLAIM_RUN_PAGES; k++) {
        run[k * PAGE_BYTES] = (char)(k & 0xff);
    }
    uint64_t t0 = now_us();
    int urc = munmap(p, sz_32g);
    uint64_t t1 = now_us();

    uint64_t after = 0;
    size_t bytes_after = 0;
    int rc1 = scan_log_max_early_table_free(&after, &bytes_after);
    uint64_t delta = after >= before ? after - before : 0;
    printf("pt-order log_rc_before=%d log_rc_after=%d bytes_before=%zu bytes_after=%zu unmap_rc=%d unmap_us=%llu before=%llu after=%llu delta=%llu\n",
           rc0, rc1, bytes_before, bytes_after, urc, (unsigned long long)(t1 - t0),
           (unsigned long long)before, (unsigned long long)after, (unsigned long long)delta);
    fflush(stdout);
    return (rc0 == 0 && rc1 == 0 && bytes_before > 0 && bytes_after > 0 && urc == 0) ? 0 : 1;
}

struct mt_worker_arg {
    unsigned char *base;
    size_t len;
    unsigned int id;
    atomic_int *stop;
    uint64_t touches;
};

static void *mt_touch_worker(void *raw) {
    struct mt_worker_arg *arg = (struct mt_worker_arg *)raw;
    assert(arg != NULL);
    assert(arg->base != NULL);
    size_t pages = arg->len / PAGE_BYTES;
    assert(pages > 0);
    uint64_t count = 0;
    for (size_t iter = 0; iter < MT_MAX_ITERS; iter++) {
        if (atomic_load_explicit(arg->stop, memory_order_relaxed) != 0) {
            break;
        }
        size_t pg = (iter + arg->id * 7) % pages;
        volatile unsigned char *cell = arg->base + pg * PAGE_BYTES;
        unsigned char v = (unsigned char)((iter + arg->id) & 0xff);
        *cell = v;
        if (*cell != v) {
            _exit(31);
        }
        count++;
    }
    arg->touches = count;
    return NULL;
}

static int run_mt_smoke(void) {
    const size_t slot_sz = 4ULL * 1024ULL * 1024ULL;
    const size_t total_sz = (MT_WORKERS + 4) * slot_sz;
    unsigned char *region = mmap(NULL, total_sz, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(region != MAP_FAILED);
    for (size_t i = 0; i < MT_WORKERS; i++) {
        assert(mprotect(region + i * slot_sz, slot_sz, PROT_READ | PROT_WRITE) == 0);
    }

    atomic_int stop = 0;
    pthread_t threads[MT_WORKERS];
    struct mt_worker_arg args[MT_WORKERS];
    for (size_t i = 0; i < MT_WORKERS; i++) {
        args[i].base = region + i * slot_sz;
        args[i].len = slot_sz;
        args[i].id = (unsigned int)i;
        args[i].stop = &stop;
        args[i].touches = 0;
        int rc = pthread_create(&threads[i], NULL, mt_touch_worker, &args[i]);
        assert(rc == 0);
    }

    unsigned char *churn_base = region + MT_WORKERS * slot_sz;
    uint64_t deadline = now_us() + (uint64_t)MT_DURATION_MS * 1000ULL;
    uint64_t churn_iters = 0;
    for (size_t iter = 0; iter < MT_MAX_ITERS; iter++) {
        if (now_us() >= deadline) {
            break;
        }
        unsigned char *slot = churn_base + (iter % 4) * slot_sz;
        void *m = mmap(slot, slot_sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
        assert(m == slot);
        for (size_t pg = 0; pg < 32; pg++) {
            slot[pg * PAGE_BYTES] = (unsigned char)((iter + pg) & 0xff);
        }
        assert(mprotect(slot, slot_sz, PROT_READ) == 0);
        assert(munmap(slot, slot_sz) == 0);
        churn_iters++;
    }

    atomic_store_explicit(&stop, 1, memory_order_relaxed);
    uint64_t total_touches = 0;
    for (size_t i = 0; i < MT_WORKERS; i++) {
        int rc = pthread_join(threads[i], NULL);
        assert(rc == 0);
        total_touches += args[i].touches;
    }
    int urc = munmap(region, MT_WORKERS * slot_sz);
    printf("mt-smoke ok=%d workers=%d churn_iters=%llu total_touches=%llu unmap_rc=%d\n",
           urc == 0 ? 1 : 0, MT_WORKERS, (unsigned long long)churn_iters,
           (unsigned long long)total_touches, urc);
    fflush(stdout);
    return urc == 0 ? 0 : 1;
}

static void abort_catching_handler(int sig) {
    (void)sig;
    static const char msg[] = "caught\n";
    ssize_t unused = write(STDOUT_FILENO, msg, sizeof(msg) - 1);
    (void)unused;
}

static int run_madvise_capture_stderr(void *addr, size_t len, int advice, int *out_errno, size_t *out_stderr_bytes) {
    int err_pipe[2];
    assert(pipe(err_pipe) == 0);
    int saved_stderr = dup(STDERR_FILENO);
    assert(saved_stderr >= 0);
    assert(dup2(err_pipe[1], STDERR_FILENO) >= 0);
    close(err_pipe[1]);
    fcntl(err_pipe[0], F_SETFL, O_NONBLOCK);

    errno = 0;
    int rc = madvise(addr, len, advice);
    *out_errno = errno;

    assert(dup2(saved_stderr, STDERR_FILENO) >= 0);
    close(saved_stderr);

    char cap[MAX_CAPTURE_BYTES];
    size_t cap_len = 0;
    drain_nonblock(err_pipe[0], cap, &cap_len, sizeof(cap));
    close(err_pipe[0]);
    *out_stderr_bytes = cap_len;
    return rc;
}

static void probe_discard_advice(const char *name, int advice) {
    size_t sz = (size_t)DISCARD_PAGES * PAGE_BYTES;
    unsigned char *p = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(p != MAP_FAILED);
    memset(p, 0x5A, sz);

    struct mem_frames f0 = read_mem_frames();
    int err = 0;
    size_t stderr_bytes = 0;
    int rc = run_madvise_capture_stderr(p, sz, advice, &err, &stderr_bytes);
    struct mem_frames f1 = read_mem_frames();

    int all_zero = 1;
    for (size_t pg = 0; pg < DISCARD_PAGES; pg++) {
        if (p[pg * PAGE_BYTES] != 0 || p[pg * PAGE_BYTES + PAGE_BYTES - 1] != 0) {
            all_zero = 0;
            break;
        }
    }
    p[0] = 0x33;
    p[sz - 1] = 0x44;
    int rewritable = (p[0] == 0x33 && p[sz - 1] == 0x44) ? 1 : 0;
    assert(munmap(p, sz) == 0);

    printf("discard-advice name=%s rc=%d errno=%d stderr_bytes=%zu frames_delta=%lld all_zero=%d rewritable=%d\n",
           name, rc, err, stderr_bytes, (long long)((int64_t)f1.used - (int64_t)f0.used), all_zero, rewritable);
    fflush(stdout);
}

static void probe_abort_case(const char *label, int mode) {
    int out_pipe[2];
    int err_pipe[2];
    assert(pipe(out_pipe) == 0);
    assert(pipe(err_pipe) == 0);

    pid_t pid = fork();
    assert(pid >= 0);
    if (pid == 0) {
        dup2(out_pipe[1], STDOUT_FILENO);
        dup2(err_pipe[1], STDERR_FILENO);
        close(out_pipe[0]);
        close(out_pipe[1]);
        close(err_pipe[0]);
        close(err_pipe[1]);
        if (mode == 1) {
            sigset_t mask;
            sigemptyset(&mask);
            sigaddset(&mask, SIGABRT);
            sigprocmask(SIG_BLOCK, &mask, NULL);
        } else if (mode == 2) {
            signal(SIGABRT, SIG_IGN);
        } else if (mode == 3) {
            struct sigaction sa;
            memset(&sa, 0, sizeof(sa));
            sa.sa_handler = abort_catching_handler;
            sigemptyset(&sa.sa_mask);
            sigaction(SIGABRT, &sa, NULL);
        }
        abort();
    }

    close(out_pipe[1]);
    close(err_pipe[1]);
    fcntl(out_pipe[0], F_SETFL, O_NONBLOCK);
    fcntl(err_pipe[0], F_SETFL, O_NONBLOCK);

    bool timed_out = false;
    int status = wait_child_status(pid, 5000, &timed_out);
    char out_buf[MAX_CAPTURE_BYTES + 1];
    char err_buf[MAX_CAPTURE_BYTES + 1];
    size_t out_len = 0;
    size_t err_len = 0;
    drain_nonblock(out_pipe[0], out_buf, &out_len, MAX_CAPTURE_BYTES);
    drain_nonblock(err_pipe[0], err_buf, &err_len, MAX_CAPTURE_BYTES);
    out_buf[out_len] = '\0';
    err_buf[err_len] = '\0';
    close(out_pipe[0]);
    close(err_pipe[0]);

    char esc_out[MAX_ESCAPED_BYTES];
    char esc_err[MAX_ESCAPED_BYTES];
    escape_text(out_buf, out_len, esc_out, sizeof(esc_out));
    escape_text(err_buf, err_len, esc_err, sizeof(esc_err));
    printf("discard-abort case=%s status=%d timed_out=%d stdout_bytes=%zu stderr_bytes=%zu stdout=\"%s\" stderr=\"%s\"\n",
           label, status, timed_out ? 1 : 0, out_len, err_len, esc_out, esc_err);
    fflush(stdout);
}

static int run_discard(void) {
    probe_discard_advice("MADV_DONTNEED", MADV_DONTNEED);
    probe_discard_advice("MADV_DONTNEED_LOCKED", MADV_DONTNEED_LOCKED);
    probe_discard_advice("MADV_FREE", MADV_FREE);

    size_t sz = (size_t)DISCARD_PAGES * PAGE_BYTES;
    unsigned char *sub = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(sub != MAP_FAILED);
    memset(sub, 0x77, sz);
    int sub_err = 0;
    size_t sub_stderr = 0;
    int sub_rc = run_madvise_capture_stderr(sub + 16 * PAGE_BYTES, 32 * PAGE_BYTES, MADV_DONTNEED, &sub_err, &sub_stderr);
    int outer_kept = 1;
    int inner_zero = 1;
    for (size_t pg = 0; pg < DISCARD_PAGES; pg++) {
        unsigned char expected = (pg >= 16 && pg < 48) ? 0x00 : 0x77;
        if (sub[pg * PAGE_BYTES] != expected || sub[pg * PAGE_BYTES + PAGE_BYTES - 1] != expected) {
            if (pg >= 16 && pg < 48) {
                inner_zero = 0;
            } else {
                outer_kept = 0;
            }
        }
    }
    assert(munmap(sub, sz) == 0);
    printf("discard-subrange rc=%d errno=%d stderr_bytes=%zu outer_kept=%d inner_zero=%d\n",
           sub_rc, sub_err, sub_stderr, outer_kept, inner_zero);

    unsigned char *pn = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(pn != MAP_FAILED);
    memset(pn, 0x99, sz);
    assert(mprotect(pn, sz, PROT_NONE) == 0);
    struct mem_frames pnf0 = read_mem_frames();
    int pn_err = 0;
    size_t pn_stderr = 0;
    int pn_rc = run_madvise_capture_stderr(pn, sz, MADV_DONTNEED_LOCKED, &pn_err, &pn_stderr);
    struct mem_frames pnf1 = read_mem_frames();
    pid_t pn_pid = fork();
    assert(pn_pid >= 0);
    if (pn_pid == 0) {
        volatile unsigned char v = pn[0];
        _exit((int)(10 + (v != 0 ? 1 : 0)));
    }
    bool pn_to = false;
    int pn_read_status = wait_child_status(pn_pid, 5000, &pn_to);
    assert(mprotect(pn, sz, PROT_READ | PROT_WRITE) == 0);
    int pn_zero = 1;
    for (size_t pg = 0; pg < DISCARD_PAGES; pg++) {
        if (pn[pg * PAGE_BYTES] != 0) {
            pn_zero = 0;
            break;
        }
    }
    assert(munmap(pn, sz) == 0);
    printf("discard-prot-none rc=%d errno=%d stderr_bytes=%zu frames_delta=%lld read_status=%d all_zero=%d\n",
           pn_rc, pn_err, pn_stderr, (long long)((int64_t)pnf1.used - (int64_t)pnf0.used), pn_read_status, pn_zero);

    unsigned char *pm = mmap(NULL, 4 * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(pm != MAP_FAILED);
    memset(pm, 0x42, 4 * PAGE_BYTES);
    int pm_rc = posix_madvise(pm, 4 * PAGE_BYTES, POSIX_MADV_DONTNEED);
    int pm_kept = (pm[0] == 0x42 && pm[4 * PAGE_BYTES - 1] == 0x42) ? 1 : 0;
    assert(munmap(pm, 4 * PAGE_BYTES) == 0);
    printf("discard-posix-madvise rc=%d data_kept=%d\n", pm_rc, pm_kept);

    unsigned char *sh = mmap(NULL, 4 * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    assert(sh != MAP_FAILED);
    memset(sh, 0x66, 4 * PAGE_BYTES);
    int sh_err = 0;
    size_t sh_stderr = 0;
    int sh_rc = run_madvise_capture_stderr(sh, 4 * PAGE_BYTES, MADV_DONTNEED, &sh_err, &sh_stderr);
    int sh_kept = (sh[0] == 0x66 && sh[4 * PAGE_BYTES - 1] == 0x66) ? 1 : 0;
    assert(munmap(sh, 4 * PAGE_BYTES) == 0);
    printf("discard-shared rc=%d errno=%d stderr_bytes=%zu data_kept=%d\n", sh_rc, sh_err, sh_stderr, sh_kept);

    unsigned char *eb = mmap(NULL, 8 * PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(eb != MAP_FAILED);
    memset(eb, 0x88, 8 * PAGE_BYTES);
    int e_unalign = 0;
    int e_zero = 0;
    int e_bad = 0;
    int e_wipe = 0;
    int e_hole = 0;
    size_t e_sb = 0;
    int r_unalign = run_madvise_capture_stderr(eb + 1, PAGE_BYTES, MADV_DONTNEED, &e_unalign, &e_sb);
    int r_zero = run_madvise_capture_stderr(eb, 0, MADV_DONTNEED, &e_zero, &e_sb);
    int r_bad = run_madvise_capture_stderr(eb, PAGE_BYTES, 999, &e_bad, &e_sb);
    int r_wipe = run_madvise_capture_stderr(eb, PAGE_BYTES, 18, &e_wipe, &e_sb);
    assert(munmap(eb + 3 * PAGE_BYTES, 2 * PAGE_BYTES) == 0);
    int r_hole = run_madvise_capture_stderr(eb, 8 * PAGE_BYTES, MADV_DONTNEED, &e_hole, &e_sb);
    int hole_zero = (eb[0] == 0 && eb[2 * PAGE_BYTES] == 0 && eb[5 * PAGE_BYTES] == 0 && eb[7 * PAGE_BYTES] == 0) ? 1 : 0;
    assert(munmap(eb, 3 * PAGE_BYTES) == 0);
    assert(munmap(eb + 5 * PAGE_BYTES, 3 * PAGE_BYTES) == 0);
    printf("discard-errors unalign=%d:%d zero=%d:%d bad=%d:%d wipe=%d:%d hole=%d:%d hole_zero=%d\n",
           r_unalign, e_unalign, r_zero, e_zero, r_bad, e_bad, r_wipe, e_wipe, r_hole, e_hole, hole_zero);

    unsigned char *sp = mmap(NULL, (size_t)SPARSE_DISCARD_BYTES, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(sp != MAP_FAILED);
    unsigned char *sp_mid = sp + (size_t)SPARSE_DISCARD_BYTES / 2;
    assert(mprotect(sp_mid, PAGE_BYTES, PROT_READ | PROT_WRITE) == 0);
    sp_mid[0] = 0xAB;
    uint64_t sp_t0 = now_us();
    int sp_err = 0;
    size_t sp_stderr = 0;
    int sp_rc = run_madvise_capture_stderr(sp, (size_t)SPARSE_DISCARD_BYTES, MADV_DONTNEED, &sp_err, &sp_stderr);
    uint64_t sp_t1 = now_us();
    int sp_zero = (sp_mid[0] == 0x00) ? 1 : 0;
    assert(munmap(sp, (size_t)SPARSE_DISCARD_BYTES) == 0);
    printf("discard-sparse rc=%d errno=%d elapsed_us=%llu zero=%d\n",
           sp_rc, sp_err, (unsigned long long)(sp_t1 - sp_t0), sp_zero);

    probe_abort_case("default", 0);
    probe_abort_case("blocked", 1);
    probe_abort_case("ignored", 2);
    probe_abort_case("caught", 3);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: hardened-malloc-probe <sparse|sparse-ops|pt-order|mt-smoke|prot-none|syscalls|mutex-stress|mutex-bench|micro|discard|run-suite DIR|timed-exec MS CMD...>\n");
        return 2;
    }
    if (strcmp(argv[1], "sparse") == 0) {
        return run_sparse();
    }
    if (strcmp(argv[1], "sparse-ops") == 0) {
        return run_sparse_ops();
    }
    if (strcmp(argv[1], "pt-order") == 0) {
        return run_pt_order();
    }
    if (strcmp(argv[1], "mt-smoke") == 0) {
        return run_mt_smoke();
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
    if (strcmp(argv[1], "discard") == 0) {
        return run_discard();
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
