#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
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

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: hardened-malloc-probe <sparse|prot-none|syscalls|run-suite DIR|timed-exec MS CMD...>\n");
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
