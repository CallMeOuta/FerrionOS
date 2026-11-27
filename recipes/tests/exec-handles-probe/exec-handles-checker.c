#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define AT_REDOX_PROC_FD 41UL
#define AT_REDOX_THR_FD 42UL
#define AT_REDOX_NS_FD 43UL
#define AT_REDOX_CWD_FD 44UL
#define AT_REDOX_FILETABLE_FD 45UL

#define UPPER_FDTBL_TAG ((size_t)1 << (sizeof(size_t) * 8 - 2))
#define MAX_AUXV_HANDLES 5
#define MAX_ENVP_ENTRIES 4096U
#define MAX_AUXV_SCAN 128U
#define MAX_TABLE_FDS 65536
#define READ_CHUNK_FDS 256
#define MAX_READ_ITERS 1024
#define MAX_PRINT_EXTRA 32
#define PATH_BUF_SIZE 256
#define PS_BUF_SIZE 65536
#define PS_MAX_READS 256
#define SIBLING_SAMPLE_COUNT 20
#define SIBLING_MID_SAMPLE 10
#define SIBLING_SAMPLE_NS 50000000L
#define FORK_REAP_POLLS 30
#define FORK_REAP_POLL_NS 10000000L
#define FORK_WAIT_BLOCK_TRIES 4
#define FORK_ALARM_SEC 1
#define MAX_ORPHAN_PIDS 16
#define MAX_TIMING_REAPS 256
#define ERRNO_CUTOFF ((size_t)-4096)

static volatile sig_atomic_t g_alarm_fired = 0;

static void on_sigalrm(int sig)
{
    (void)sig;
    g_alarm_fired = 1;
}

extern size_t redox_cur_thrfd_v0(void);
extern size_t redox_dup_v1(size_t fd, const uint8_t *buf, size_t len);
extern size_t redox_read_v1(size_t fd, uint8_t *dst_base, size_t dst_len);
extern size_t redox_fpath_v1(size_t fd, uint8_t *dst_base, size_t dst_len);
extern size_t redox_close_v1(size_t fd);

static int is_raw_err(size_t r)
{
    return r >= ERRNO_CUTOFF;
}

static int raw_errno(size_t r)
{
    return (int)(0 - r);
}

static int in_set(const size_t *set, size_t n, size_t val)
{
    for (size_t i = 0; i < n; i++) {
        if (set[i] == val) {
            return 1;
        }
    }
    return 0;
}

static int in_u64_set(const uint64_t *set, size_t n, uint64_t val)
{
    for (size_t i = 0; i < n; i++) {
        if (set[i] == val) {
            return 1;
        }
    }
    return 0;
}

static uintptr_t initial_stack_sp = 0;

extern void __real_relibc_start_v1(uintptr_t sp, void *main_fn) __attribute__((noreturn));

void __wrap_relibc_start_v1(uintptr_t sp, void *main_fn)
{
    initial_stack_sp = sp;
    __real_relibc_start_v1(sp, main_fn);
}

static unsigned long read_initial_auxv(unsigned long key)
{
    assert(initial_stack_sp != 0);
    const uintptr_t *p = (const uintptr_t *)initial_stack_sp;
    size_t argc = (size_t)p[0];
    p += 1 + argc + 1;
    int env_end = 0;
    for (size_t i = 0; i < MAX_ENVP_ENTRIES; i++) {
        if (*p == 0) {
            p++;
            env_end = 1;
            break;
        }
        p++;
    }
    assert(env_end);
    for (size_t i = 0; i < MAX_AUXV_SCAN; i++) {
        unsigned long k = (unsigned long)p[0];
        unsigned long v = (unsigned long)p[1];
        if (k == 0UL) {
            return 0UL;
        }
        if (k == key) {
            return v;
        }
        p += 2;
    }
    return 0UL;
}

static void add_auxv_handle(size_t *set, size_t *count, unsigned long key)
{
    unsigned long v = read_initial_auxv(key);
    if (v == 0UL || v == ~0UL) {
        return;
    }
    size_t fd = (size_t)v;
    if ((fd & UPPER_FDTBL_TAG) == 0) {
        return;
    }
    assert(*count < MAX_AUXV_HANDLES);
    if (!in_set(set, *count, fd)) {
        set[*count] = fd;
        (*count)++;
    }
}

static int read_filetable(size_t thr_fd, uint64_t *fds, size_t *count_out, size_t *self_fd_out)
{
    static const uint8_t ft_name[] = "filetable-binary";
    size_t ft_res = redox_dup_v1(thr_fd, ft_name, sizeof(ft_name) - 1);
    if (is_raw_err(ft_res)) {
        return -raw_errno(ft_res);
    }

    size_t total_bytes = 0;
    const size_t max_bytes = MAX_TABLE_FDS * sizeof(uint64_t);
    int read_ok = 0;
    for (int iter = 0; iter < MAX_READ_ITERS; iter++) {
        size_t want = READ_CHUNK_FDS * sizeof(uint64_t);
        if (want > max_bytes - total_bytes) {
            want = max_bytes - total_bytes;
        }
        if (want == 0) {
            break;
        }
        size_t r = redox_read_v1(ft_res, ((uint8_t *)fds) + total_bytes, want);
        if (is_raw_err(r)) {
            int err = raw_errno(r);
            if (err == EINTR) {
                continue;
            }
            (void)redox_close_v1(ft_res);
            return -err;
        }
        if (r == 0) {
            read_ok = 1;
            break;
        }
        total_bytes += r;
    }

    if (!read_ok || (total_bytes % sizeof(uint64_t)) != 0) {
        (void)redox_close_v1(ft_res);
        return -EIO;
    }

    *count_out = total_bytes / sizeof(uint64_t);
    *self_fd_out = ft_res;
    return 0;
}

static int cmp_u64(const void *a, const void *b)
{
    uint64_t ua = *(const uint64_t *)a;
    uint64_t ub = *(const uint64_t *)b;
    return (ua > ub) - (ua < ub);
}

static size_t copy_without_fd(const uint64_t *src, size_t n, size_t skip_fd, uint64_t *dst)
{
    size_t out = 0;
    for (size_t i = 0; i < n; i++) {
        if ((size_t)src[i] == skip_fd) {
            continue;
        }
        dst[out++] = src[i];
    }
    if (out > 1) {
        qsort(dst, out, sizeof(uint64_t), cmp_u64);
    }
    return out;
}

static int read_self_nthrd(void)
{
    int fd = open("/scheme/proc/ps", O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        return -1;
    }
    char *buf = malloc(PS_BUF_SIZE);
    if (!buf) {
        (void)close(fd);
        return -1;
    }
    size_t total = 0;
    for (int i = 0; i < PS_MAX_READS && total + 1 < PS_BUF_SIZE; i++) {
        ssize_t n = read(fd, buf + total, PS_BUF_SIZE - 1 - total);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            free(buf);
            (void)close(fd);
            return -1;
        }
        if (n == 0) {
            break;
        }
        total += (size_t)n;
    }
    (void)close(fd);
    buf[total] = '\0';

    long my_pid = (long)getpid();
    int found = -1;
    char *p = buf;
    for (size_t line_no = 0; line_no < 4096 && *p != '\0'; line_no++) {
        char *nl = strchr(p, '\n');
        if (nl) {
            *nl = '\0';
        }
        long pid = -1;
        long pgid = 0, ppid = 0, sid = 0, ruid = 0, rgid = 0, euid = 0, egid = 0;
        int nthrd = -1;
        if (sscanf(p, "%ld %ld %ld %ld %ld %ld %ld %ld %d",
                &pid, &pgid, &ppid, &sid, &ruid, &rgid, &euid, &egid, &nthrd) == 9) {
            if (pid == my_pid) {
                found = nthrd;
                break;
            }
        }
        if (!nl) {
            break;
        }
        p = nl + 1;
    }
    free(buf);
    return found;
}

static int find_ps_children(long parent_pid, pid_t *out_pids, int max_pids, char *first_status, size_t status_cap)
{
    int fd = open("/scheme/proc/ps", O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        return -1;
    }
    char *buf = malloc(PS_BUF_SIZE);
    if (!buf) {
        (void)close(fd);
        return -1;
    }
    size_t total = 0;
    for (int i = 0; i < PS_MAX_READS && total + 1 < PS_BUF_SIZE; i++) {
        ssize_t n = read(fd, buf + total, PS_BUF_SIZE - 1 - total);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            free(buf);
            (void)close(fd);
            return -1;
        }
        if (n == 0) {
            break;
        }
        total += (size_t)n;
    }
    (void)close(fd);
    buf[total] = '\0';

    int count = 0;
    if (status_cap > 0) {
        first_status[0] = '\0';
    }
    char *p = buf;
    for (size_t line_no = 0; line_no < 4096 && *p != '\0'; line_no++) {
        char *nl = strchr(p, '\n');
        if (nl) {
            *nl = '\0';
        }
        long pid = -1;
        long pgid = 0, ppid = 0, sid = 0, ruid = 0, rgid = 0, euid = 0, egid = 0;
        int nthrd = -1;
        char status_tok[16];
        status_tok[0] = '\0';
        if (sscanf(p, "%ld %ld %ld %ld %ld %ld %ld %ld %d %15s",
                &pid, &pgid, &ppid, &sid, &ruid, &rgid, &euid, &egid, &nthrd, status_tok) == 10) {
            if (ppid == parent_pid && pid != parent_pid) {
                if (count < max_pids) {
                    out_pids[count] = (pid_t)pid;
                }
                if (count == 0 && status_cap > 0) {
                    snprintf(first_status, status_cap, "%s", status_tok);
                }
                count++;
            }
        }
        if (!nl) {
            break;
        }
        p = nl + 1;
    }
    free(buf);
    return count;
}

static int run_fork_orphan_check(const char *round_str)
{
    int nthrd = read_self_nthrd();
    if (nthrd != 1) {
        fprintf(stdout, "exec-handles-checker: fork-orphan-nthrd round=%s nthrd=%d\n", round_str, nthrd);
        fflush(stdout);
        return 44;
    }

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_sigalrm;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    int sra = sigaction(SIGALRM, &sa, NULL);
    assert(sra == 0);

    int reaped_started = 0;
    struct timespec poll_req;
    poll_req.tv_sec = 0;
    poll_req.tv_nsec = FORK_REAP_POLL_NS;
    for (int i = 0; i < FORK_REAP_POLLS; i++) {
        int st = 0;
        pid_t w = waitpid(-1, &st, WNOHANG);
        if (w > 0) {
            reaped_started++;
            continue;
        }
        if (w < 0 && errno == ECHILD) {
            break;
        }
        if (w < 0 && errno == EINTR) {
            continue;
        }
        (void)nanosleep(&poll_req, NULL);
    }

    pid_t orphan_pids[MAX_ORPHAN_PIDS];
    char orphan_status[16];
    int ps_children = find_ps_children((long)getpid(), orphan_pids, MAX_ORPHAN_PIDS, orphan_status, sizeof(orphan_status));

    int waitpid_blocked = 0;
    int waitpid_echild = 0;
    for (int k = 0; k < FORK_WAIT_BLOCK_TRIES; k++) {
        g_alarm_fired = 0;
        (void)alarm(FORK_ALARM_SEC);
        int bst = 0;
        pid_t bw = waitpid(-1, &bst, 0);
        int berr = errno;
        (void)alarm(0);
        if (bw > 0) {
            reaped_started++;
            continue;
        }
        if (bw < 0 && berr == ECHILD) {
            waitpid_echild = 1;
            break;
        }
        if (bw < 0 && berr == EINTR && g_alarm_fired) {
            waitpid_blocked = 1;
            break;
        }
    }

    if (ps_children > 0 && waitpid_echild) {
        ps_children = find_ps_children((long)getpid(), orphan_pids, MAX_ORPHAN_PIDS, orphan_status, sizeof(orphan_status));
    }

    if (ps_children != 0 || !waitpid_echild || waitpid_blocked) {
        long first_pid = (ps_children > 0) ? (long)orphan_pids[0] : -1L;
        const char *st_str = (ps_children > 0 && orphan_status[0] != '\0') ? orphan_status : "none";
        fprintf(stdout,
            "exec-handles-checker: fork-orphan round=%s pid=%ld ps_children=%d orphan_pid=%ld orphan_status=%s waitpid_blocked=%d waitpid_echild=%d reaped_started=%d\n",
            round_str, (long)getpid(), ps_children, first_pid, st_str, waitpid_blocked, waitpid_echild, reaped_started);
        fflush(stdout);
        return 47;
    }

    return 0;
}

static uint64_t checker_now_us(void)
{
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000000ULL + (uint64_t)(ts.tv_nsec / 1000L);
}

static int run_timing_stamp(const char *t0_str, const char *fork_us_str)
{
    uint64_t t1 = checker_now_us();
    uint64_t t0 = strtoull(t0_str, NULL, 10);
    uint64_t dt = (t1 >= t0) ? (t1 - t0) : 0ULL;
    if (fork_us_str != NULL) {
        fprintf(stdout, "exec-handles-checker: timing_us=%llu fork_100_us=%s\n",
            (unsigned long long)dt, fork_us_str);
    } else {
        fprintf(stdout, "exec-handles-checker: timing_us=%llu\n", (unsigned long long)dt);
    }
    fflush(stdout);
    int reaped = 0;
    for (int i = 0; i < MAX_TIMING_REAPS; i++) {
        int st = 0;
        pid_t w = waitpid(-1, &st, 0);
        if (w > 0) {
            reaped++;
            continue;
        }
        if (w < 0 && errno == EINTR) {
            continue;
        }
        if (w < 0 && errno == ECHILD) {
            break;
        }
        return 1;
    }
    fprintf(stdout, "exec-handles-checker: timing_reaped=%d\n", reaped);
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv)
{
    if (argc >= 3 && strcmp(argv[1], "--timing-stamp") == 0) {
        return run_timing_stamp(argv[2], argc >= 4 ? argv[3] : NULL);
    }
    if (argc >= 3 && strcmp(argv[1], "--fork-orphan-check") == 0) {
        return run_fork_orphan_check(argv[2]);
    }
    const char *mode = argc > 1 ? argv[1] : "unknown";
    const char *threads = argc > 2 ? argv[2] : "0";
    int verbose = argc > 3 && strcmp(argv[3], "1") == 0;
    const char *marker_path = argc > 4 ? argv[4] : NULL;

    size_t keep[MAX_AUXV_HANDLES];
    size_t keep_count = 0;
    add_auxv_handle(keep, &keep_count, AT_REDOX_PROC_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_THR_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_NS_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_CWD_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_FILETABLE_FD);

    size_t thr_fd = (size_t)read_initial_auxv(AT_REDOX_THR_FD);
    if (thr_fd == 0 || thr_fd == ~(size_t)0) {
        thr_fd = redox_cur_thrfd_v0();
    }
    assert(thr_fd != 0);

    uint64_t *fds = calloc(MAX_TABLE_FDS, sizeof(uint64_t));
    if (!fds) {
        fprintf(stderr, "exec-handles-checker: out of memory\n");
        return 2;
    }

    size_t fd_count = 0;
    size_t ft_res = 0;
    int rrc = read_filetable(thr_fd, fds, &fd_count, &ft_res);
    if (rrc != 0) {
        fprintf(stderr, "exec-handles-checker: read filetable failed errno=%d\n", -rrc);
        free(fds);
        return 2;
    }

    size_t upper_total = 0;
    size_t extra = 0;
    size_t missing_posix = 0;
    for (size_t i = 0; i < fd_count; i++) {
        size_t fd = (size_t)fds[i];
        if ((fd & UPPER_FDTBL_TAG) == 0) {
            if (fcntl((int)fd, F_GETFD) < 0) {
                if (verbose && missing_posix < MAX_PRINT_EXTRA) {
                    fprintf(stderr, "exec-handles-checker: missing-posix mode=%s threads=%s fd=%zu errno=%d\n",
                        mode, threads, fd, errno);
                }
                missing_posix++;
            }
            continue;
        }
        upper_total++;
        if (in_set(keep, keep_count, fd)) {
            continue;
        }
        if (verbose && extra < MAX_PRINT_EXTRA) {
            uint8_t path_buf[PATH_BUF_SIZE];
            size_t pr = redox_fpath_v1(fd, path_buf, sizeof(path_buf) - 1);
            if (!is_raw_err(pr) && pr < sizeof(path_buf)) {
                path_buf[pr] = '\0';
            } else {
                snprintf((char *)path_buf, sizeof(path_buf), "<fpath-err-%d>", is_raw_err(pr) ? raw_errno(pr) : -1);
            }
            fprintf(stdout, "exec-handles-checker: extra mode=%s threads=%s slot=%zu raw=0x%zx path=%s\n",
                mode, threads, fd & ~UPPER_FDTBL_TAG, fd, (char *)path_buf);
        }
        extra++;
    }

    int open_clash = 0;
    int null_fd = open("/dev/null", O_RDONLY);
    if (null_fd < 0) {
        open_clash = 1;
        if (verbose) {
            fprintf(stderr, "exec-handles-checker: open /dev/null failed mode=%s threads=%s errno=%d\n",
                mode, threads, errno);
        }
    } else if (in_u64_set(fds, fd_count, (uint64_t)null_fd)) {
        open_clash = 1;
        if (verbose) {
            fprintf(stderr, "exec-handles-checker: open-clash mode=%s threads=%s fd=%d\n",
                mode, threads, null_fd);
        }
    } else {
        (void)close(null_fd);
    }

    (void)redox_close_v1(ft_res);

    if (verbose) {
        FILE *out = open_clash ? stderr : stdout;
        fprintf(out, "exec-handles-checker: summary mode=%s threads=%s auxv=%zu upper=%zu extra=%zu missing_posix=%zu open_clash=%d\n",
            mode, threads, keep_count, upper_total, extra, missing_posix, open_clash);
    }

    int sib_rc = 0;
    if (marker_path != NULL) {
        int nthrd_start = read_self_nthrd();
        struct stat st_start;
        long long size_start = (stat(marker_path, &st_start) == 0) ? (long long)st_start.st_size : -1LL;

        uint64_t *base_fds = calloc(MAX_TABLE_FDS, sizeof(uint64_t));
        uint64_t *cur_raw = calloc(MAX_TABLE_FDS, sizeof(uint64_t));
        uint64_t *cur_fds = calloc(MAX_TABLE_FDS, sizeof(uint64_t));
        if (!base_fds || !cur_raw || !cur_fds) {
            free(base_fds);
            free(cur_raw);
            free(cur_fds);
            free(fds);
            return 2;
        }
        size_t base_n = copy_without_fd(fds, fd_count, ft_res, base_fds);
        int nthrd_mid = -1;
        int ft_changed = 0;

        struct timespec req;
        req.tv_sec = 0;
        req.tv_nsec = SIBLING_SAMPLE_NS;
        for (int s = 1; s <= SIBLING_SAMPLE_COUNT; s++) {
            (void)nanosleep(&req, NULL);
            if (s == SIBLING_MID_SAMPLE) {
                nthrd_mid = read_self_nthrd();
            }
            size_t cur_raw_n = 0;
            size_t cur_ft = 0;
            if (read_filetable(thr_fd, cur_raw, &cur_raw_n, &cur_ft) != 0) {
                ft_changed = 1;
            } else {
                (void)redox_close_v1(cur_ft);
                size_t cur_n = copy_without_fd(cur_raw, cur_raw_n, cur_ft, cur_fds);
                if (cur_n != base_n || (cur_n > 0 && memcmp(base_fds, cur_fds, cur_n * sizeof(uint64_t)) != 0)) {
                    ft_changed = 1;
                }
            }
        }

        struct stat st_end;
        long long size_end = (stat(marker_path, &st_end) == 0) ? (long long)st_end.st_size : -1LL;
        int grew = (size_start < 0 || size_end < 0 || size_end != size_start);

        if (verbose) {
            FILE *out = open_clash ? stderr : stdout;
            fprintf(out, "exec-handles-checker: siblings mode=%s nthrd_start=%d nthrd_mid=%d size_start=%lld size_end=%lld ft_changed=%d\n",
                mode, nthrd_start, nthrd_mid, size_start, size_end, ft_changed);
        }

        if (nthrd_start != 1 || nthrd_mid != 1) {
            sib_rc = 44;
        } else if (grew) {
            sib_rc = 45;
        } else if (ft_changed) {
            sib_rc = 46;
        }

        free(base_fds);
        free(cur_raw);
        free(cur_fds);
    }

    free(fds);
    if (extra > 0) {
        return 42;
    }
    if (missing_posix > 0 || open_clash > 0) {
        return 43;
    }
    return sib_rc;
}
