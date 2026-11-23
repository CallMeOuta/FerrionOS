#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <unistd.h>

#define AT_REDOX_PROC_FD 41UL
#define AT_REDOX_THR_FD 42UL
#define AT_REDOX_NS_FD 43UL
#define AT_REDOX_CWD_FD 44UL
#define AT_REDOX_FILETABLE_FD 45UL

#define UPPER_FDTBL_TAG ((size_t)1 << (sizeof(size_t) * 8 - 2))
#define MAX_AUXV_HANDLES 5
#define MAX_TABLE_FDS 65536
#define READ_CHUNK_FDS 256
#define MAX_READ_ITERS 1024
#define MAX_PRINT_EXTRA 32
#define PATH_BUF_SIZE 256
#define ERRNO_CUTOFF ((size_t)-4096)

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

static void add_auxv_handle(size_t *set, size_t *count, unsigned long key)
{
    unsigned long v = getauxval(key);
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

int main(int argc, char **argv)
{
    const char *mode = argc > 1 ? argv[1] : "unknown";
    const char *threads = argc > 2 ? argv[2] : "0";
    int verbose = argc > 3 && strcmp(argv[3], "1") == 0;

    size_t keep[MAX_AUXV_HANDLES];
    size_t keep_count = 0;
    add_auxv_handle(keep, &keep_count, AT_REDOX_PROC_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_THR_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_NS_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_CWD_FD);
    add_auxv_handle(keep, &keep_count, AT_REDOX_FILETABLE_FD);

    size_t thr_fd = (size_t)getauxval(AT_REDOX_THR_FD);
    if (thr_fd == 0 || thr_fd == ~(size_t)0) {
        thr_fd = redox_cur_thrfd_v0();
    }
    assert(thr_fd != 0);

    static const uint8_t ft_name[] = "filetable-binary";
    size_t ft_res = redox_dup_v1(thr_fd, ft_name, sizeof(ft_name) - 1);
    if (is_raw_err(ft_res)) {
        fprintf(stderr, "exec-handles-checker: dup filetable-binary failed errno=%d\n", raw_errno(ft_res));
        return 2;
    }

    uint64_t *fds = calloc(MAX_TABLE_FDS, sizeof(uint64_t));
    if (!fds) {
        (void)redox_close_v1(ft_res);
        fprintf(stderr, "exec-handles-checker: out of memory\n");
        return 2;
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
            fprintf(stderr, "exec-handles-checker: read filetable failed errno=%d\n", err);
            free(fds);
            (void)redox_close_v1(ft_res);
            return 2;
        }
        if (r == 0) {
            read_ok = 1;
            break;
        }
        total_bytes += r;
    }

    if (!read_ok || (total_bytes % sizeof(uint64_t)) != 0) {
        (void)redox_close_v1(ft_res);
        fprintf(stderr, "exec-handles-checker: incomplete filetable read bytes=%zu\n", total_bytes);
        free(fds);
        return 2;
    }

    size_t fd_count = total_bytes / sizeof(uint64_t);
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

    free(fds);
    if (extra > 0) {
        return 42;
    }
    if (missing_posix > 0 || open_clash > 0) {
        return 43;
    }
    return 0;
}
