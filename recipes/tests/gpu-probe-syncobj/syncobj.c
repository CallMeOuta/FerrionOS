#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

#define TAG "gpu-probe-syncobj"

enum out_result {
    OUT_REFUSED,
    OUT_MISSING,
    OUT_OK,
};

#define IOC_NR(dir, type, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | ((unsigned long)(type) << 8) | (nr)))
#define DRM_IOWR(nr, t) IOC_NR(3, 'd', nr, sizeof(t))
#define SYNC_IOWR(nr, t) IOC_NR(3, '>', nr, sizeof(t))
#define DMABUF_IOWR(nr, t) IOC_NR(3, 'b', nr, sizeof(t))

#define DRM_CAP_SYNCOBJ 19
#define DRM_CAP_SYNCOBJ_TIMELINE 20

#define DRM_SYNCOBJ_CREATE_SIGNALED (1U << 0)
#define DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE (1U << 0)
#define DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_TIMELINE (1U << 1)
#define DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE (1U << 0)
#define DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE (1U << 1)
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL (1U << 0)
#define DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT (1U << 1)

struct drm_get_cap {
    uint64_t capability;
    uint64_t value;
};

struct drm_syncobj_create {
    uint32_t handle;
    uint32_t flags;
};

struct drm_syncobj_destroy {
    uint32_t handle;
    uint32_t pad;
};

struct drm_syncobj_handle {
    uint32_t handle;
    uint32_t flags;
    int32_t fd;
    uint32_t pad;
    uint64_t point;
};

struct drm_syncobj_transfer {
    uint32_t src_handle;
    uint32_t dst_handle;
    uint64_t src_point;
    uint64_t dst_point;
    uint32_t flags;
    uint32_t pad;
};

struct drm_syncobj_wait {
    uint64_t handles;
    int64_t timeout_nsec;
    uint32_t count_handles;
    uint32_t flags;
    uint32_t first_signaled;
    uint32_t pad;
    uint64_t deadline_nsec;
};

struct drm_syncobj_timeline_wait {
    uint64_t handles;
    uint64_t points;
    int64_t timeout_nsec;
    uint32_t count_handles;
    uint32_t flags;
    uint32_t first_signaled;
    uint32_t pad;
    uint64_t deadline_nsec;
};

struct drm_syncobj_eventfd {
    uint32_t handle;
    uint32_t flags;
    uint64_t point;
    int32_t fd;
    uint32_t pad;
};

struct drm_syncobj_array {
    uint64_t handles;
    uint32_t count_handles;
    uint32_t pad;
};

struct drm_syncobj_timeline_array {
    uint64_t handles;
    uint64_t points;
    uint32_t count_handles;
    uint32_t flags;
};

struct drm_mode_fb_cmd2 {
    uint32_t fb_id;
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    uint32_t flags;
    uint32_t handles[4];
    uint32_t pitches[4];
    uint32_t offsets[4];
    uint64_t modifier[4];
};

struct sync_merge_data {
    char name[32];
    int32_t fd2;
    int32_t fence;
    uint32_t flags;
    uint32_t pad;
};

struct sync_fence_info {
    char obj_name[32];
    char driver_name[32];
    int32_t status;
    uint32_t flags;
    uint64_t timestamp_ns;
};

struct sync_file_info {
    char name[32];
    int32_t status;
    uint32_t flags;
    uint32_t num_fences;
    uint32_t pad;
    uint64_t sync_fence_info;
};

struct dma_buf_export_sync_file {
    uint32_t flags;
    int32_t fd;
};

#define DRM_IOCTL_GET_CAP DRM_IOWR(0x0c, struct drm_get_cap)
#define DRM_IOCTL_SYNCOBJ_CREATE DRM_IOWR(0xbf, struct drm_syncobj_create)
#define DRM_IOCTL_SYNCOBJ_DESTROY DRM_IOWR(0xc0, struct drm_syncobj_destroy)
#define DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD DRM_IOWR(0xc1, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE DRM_IOWR(0xc2, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_WAIT DRM_IOWR(0xc3, struct drm_syncobj_wait)
#define DRM_IOCTL_SYNCOBJ_RESET DRM_IOWR(0xc4, struct drm_syncobj_array)
#define DRM_IOCTL_SYNCOBJ_SIGNAL DRM_IOWR(0xc5, struct drm_syncobj_array)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT DRM_IOWR(0xca, struct drm_syncobj_timeline_wait)
#define DRM_IOCTL_SYNCOBJ_QUERY DRM_IOWR(0xcb, struct drm_syncobj_timeline_array)
#define DRM_IOCTL_SYNCOBJ_TRANSFER DRM_IOWR(0xcc, struct drm_syncobj_transfer)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL DRM_IOWR(0xcd, struct drm_syncobj_timeline_array)
#define DRM_IOCTL_MODE_GETFB2 DRM_IOWR(0xce, struct drm_mode_fb_cmd2)
#define DRM_IOCTL_SYNCOBJ_EVENTFD DRM_IOWR(0xcf, struct drm_syncobj_eventfd)

#define SYNC_IOC_MERGE SYNC_IOWR(3, struct sync_merge_data)
#define SYNC_IOC_FILE_INFO SYNC_IOWR(4, struct sync_file_info)
#define DMA_BUF_IOCTL_EXPORT_SYNC_FILE DMABUF_IOWR(2, struct dma_buf_export_sync_file)

static int failures;

static void pass(const char *check, const char *detail)
{
    printf(TAG ": pass %s: %s\n", check, detail);
}

static void fail(const char *check, const char *detail)
{
    printf(TAG ": FAIL %s: %s\n", check, detail);
    failures++;
}

static int expect_ok(const char *check, int ret)
{
    char detail[128];
    if (ret < 0) {
        snprintf(detail, sizeof detail, "failed with %s (%d)", strerror(errno), errno);
        fail(check, detail);
    }
    return ret;
}

static enum out_result check_out(const char *check, int ret, int has_output, const char *missing_detail)
{
    if (expect_ok(check, ret) < 0)
        return OUT_REFUSED;
    if (!has_output) {
        fail(check, missing_detail);
        return OUT_MISSING;
    }
    return OUT_OK;
}

static void expect_errno(const char *check, int ret, int want)
{
    char detail[160];
    if (ret == 0) {
        snprintf(detail, sizeof detail, "accepted; expected %s (%d)", strerror(want), want);
        fail(check, detail);
    } else if (errno != want) {
        snprintf(detail, sizeof detail, "refused with %s (%d); expected %s (%d)",
                 strerror(errno), errno, strerror(want), want);
        fail(check, detail);
    } else {
        snprintf(detail, sizeof detail, "refused with %s", strerror(want));
        pass(check, detail);
    }
}

static int64_t mono_ns(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0)
        return 0;
    return (int64_t)ts.tv_sec * 1000000000LL + (int64_t)ts.tv_nsec;
}

static void sleep_ms(int ms)
{
    struct timespec req;
    req.tv_sec = ms / 1000;
    req.tv_nsec = (long)(ms % 1000) * 1000000L;
    while (nanosleep(&req, &req) < 0 && errno == EINTR)
        ;
}

static int find_render_name(char *out, size_t size)
{
    DIR *dir = opendir("/scheme");
    if (!dir)
        return 0;
    struct dirent *entry;
    out[0] = 0;
    while ((entry = readdir(dir))) {
        if (strncmp(entry->d_name, "gpu.", 4) == 0 && strlen(entry->d_name) < size &&
            (!out[0] || strcmp(entry->d_name, out) < 0))
            strcpy(out, entry->d_name);
    }
    closedir(dir);
    return out[0] != 0;
}

static int syncobj_create(int fd, uint32_t flags, uint32_t *out_handle)
{
    struct drm_syncobj_create req = {.flags = flags};
    int ret = ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &req);
    if (ret == 0 && out_handle)
        *out_handle = req.handle;
    return ret;
}

static int syncobj_destroy(int fd, uint32_t handle)
{
    struct drm_syncobj_destroy req = {.handle = handle};
    return ioctl(fd, DRM_IOCTL_SYNCOBJ_DESTROY, &req);
}

static int syncobj_timeline_signal(int fd, uint32_t handle, uint64_t point)
{
    struct drm_syncobj_timeline_array req = {
        .handles = (uintptr_t)&handle,
        .points = (uintptr_t)&point,
        .count_handles = 1,
        .flags = 0,
    };
    return ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_SIGNAL, &req);
}

static int syncobj_query(int fd, uint32_t handle, uint64_t *out_point)
{
    uint64_t point = 0;
    struct drm_syncobj_timeline_array req = {
        .handles = (uintptr_t)&handle,
        .points = (uintptr_t)&point,
        .count_handles = 1,
        .flags = 0,
    };
    int ret = ioctl(fd, DRM_IOCTL_SYNCOBJ_QUERY, &req);
    if (ret == 0 && out_point)
        *out_point = point;
    return ret;
}

static int syncobj_timeline_wait(int fd, uint32_t handle, uint64_t point, int64_t timeout_nsec,
                                 uint32_t flags, uint32_t *first_signaled)
{
    struct drm_syncobj_timeline_wait req = {
        .handles = (uintptr_t)&handle,
        .points = (uintptr_t)&point,
        .timeout_nsec = timeout_nsec,
        .count_handles = 1,
        .flags = flags,
    };
    int ret = ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &req);
    if (ret == 0 && first_signaled)
        *first_signaled = req.first_signaled;
    return ret;
}

static int sync_file_status(int sync_fd, uint32_t *out_num_fences)
{
    struct sync_file_info info = {0};
    if (ioctl(sync_fd, SYNC_IOC_FILE_INFO, &info) < 0)
        return -1;
    if (out_num_fences)
        *out_num_fences = info.num_fences;
    return info.status;
}

static int poll_readable(int fd, int timeout_ms)
{
    struct pollfd pfd = {.fd = fd, .events = POLLIN, .revents = 0};
    int ret = poll(&pfd, 1, timeout_ms);
    if (ret < 0)
        return -1;
    if (ret == 0)
        return 0;
    return (pfd.revents & POLLIN) ? 1 : -2;
}

struct waiter_args {
    int fd;
    uint32_t handle;
    uint64_t point;
    volatile int started;
    volatile int done;
    int ret;
    int err;
    int64_t wake_ns;
};

static void *timeline_waiter_thread(void *arg)
{
    struct waiter_args *w = (struct waiter_args *)arg;
    w->started = 1;
    int64_t deadline = mono_ns() + 5000000000LL;
    int ret = syncobj_timeline_wait(w->fd, w->handle, w->point, deadline,
                                    DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL |
                                        DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
                                    NULL);
    w->wake_ns = mono_ns();
    w->ret = ret;
    w->err = ret < 0 ? errno : 0;
    w->done = 1;
    return NULL;
}

struct poll_waiter_args {
    int fd;
    volatile int done;
    int ret;
    int64_t wake_ns;
};

static void *poll_waiter_thread(void *arg)
{
    struct poll_waiter_args *w = (struct poll_waiter_args *)arg;
    w->ret = poll_readable(w->fd, 5000);
    w->wake_ns = mono_ns();
    w->done = 1;
    return NULL;
}

int gpu_probe_syncobj(int argc, char **argv)
{
    char render[256], path[300], detail[256];
    (void)argc;
    (void)argv;
    setvbuf(stdout, NULL, _IOLBF, 0);

    if (!find_render_name(render, sizeof render)) {
        fail("render-name", "no gpu.* name in /scheme");
        printf(TAG ": FAIL\n");
        return 1;
    }
    printf(TAG ": render %s\n", render);
    pass("render-name", render);
    snprintf(path, sizeof path, "/scheme/%s", render);

    int first = open(path, O_RDWR | O_CLOEXEC);
    int second = open(path, O_RDWR | O_CLOEXEC);
    if (expect_ok("open-first", first) < 0 || expect_ok("open-second", second) < 0) {
        printf(TAG ": FAIL\n");
        return 1;
    }

    struct drm_get_cap cap_syncobj = {.capability = DRM_CAP_SYNCOBJ};
    if (expect_ok("cap-syncobj", ioctl(first, DRM_IOCTL_GET_CAP, &cap_syncobj)) == 0) {
        if (cap_syncobj.value == 1)
            pass("cap-syncobj", "1");
        else
            fail("cap-syncobj", "expected 1");
    }
    struct drm_get_cap cap_timeline = {.capability = DRM_CAP_SYNCOBJ_TIMELINE};
    if (expect_ok("cap-syncobj-timeline", ioctl(first, DRM_IOCTL_GET_CAP, &cap_timeline)) == 0) {
        if (cap_timeline.value == 1)
            pass("cap-syncobj-timeline", "1");
        else
            fail("cap-syncobj-timeline", "expected 1");
    }

    uint32_t bin_unsig = 0, bin_sig = 0;
    if (expect_ok("create-unsignaled", syncobj_create(first, 0, &bin_unsig)) == 0)
        pass("create-unsignaled", "created");
    if (expect_ok("create-signaled",
                  syncobj_create(first, DRM_SYNCOBJ_CREATE_SIGNALED, &bin_sig)) == 0)
        pass("create-signaled", "created");

    if (bin_unsig && bin_sig) {
        uint32_t pair[2] = {bin_unsig, bin_sig};
        struct drm_syncobj_wait w_any = {
            .handles = (uintptr_t)pair,
            .timeout_nsec = 0,
            .count_handles = 2,
            .flags = 0,
            .first_signaled = 99,
        };
        if (expect_ok("wait-any-signaled", ioctl(first, DRM_IOCTL_SYNCOBJ_WAIT, &w_any)) == 0) {
            if (w_any.first_signaled == 1)
                pass("wait-any-signaled", "first_signaled=1");
            else {
                snprintf(detail, sizeof detail, "first_signaled=%u, expected 1",
                         w_any.first_signaled);
                fail("wait-any-signaled", detail);
            }
        }

        struct drm_syncobj_wait w_all = {
            .handles = (uintptr_t)pair,
            .timeout_nsec = 0,
            .count_handles = 2,
            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
        };
        expect_errno("wait-all-unsignaled", ioctl(first, DRM_IOCTL_SYNCOBJ_WAIT, &w_all), ETIME);

        struct drm_syncobj_array sig_req = {.handles = (uintptr_t)&bin_unsig, .count_handles = 1};
        if (expect_ok("binary-signal", ioctl(first, DRM_IOCTL_SYNCOBJ_SIGNAL, &sig_req)) == 0) {
            if (expect_ok("wait-all-after-signal",
                          ioctl(first, DRM_IOCTL_SYNCOBJ_WAIT, &w_all)) == 0)
                pass("wait-all-after-signal", "both signaled");
        }

        struct drm_syncobj_array rst_req = {.handles = (uintptr_t)&bin_unsig, .count_handles = 1};
        if (expect_ok("binary-reset", ioctl(first, DRM_IOCTL_SYNCOBJ_RESET, &rst_req)) == 0) {
            expect_errno("wait-after-reset", ioctl(first, DRM_IOCTL_SYNCOBJ_WAIT, &w_all), ETIME);
        }
    }

    uint32_t tl = 0;
    if (expect_ok("create-timeline", syncobj_create(first, 0, &tl)) == 0) {
        int64_t t0 = mono_ns();
        int64_t deadline = t0 + 300000000LL;
        int ret = syncobj_timeline_wait(first, tl, 1, deadline,
                                        DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL |
                                            DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT,
                                        NULL);
        int saved_errno = errno;
        int64_t t1 = mono_ns();
        int64_t elapsed_ms = (t1 - t0) / 1000000LL;
        printf(TAG ": timeout-ms %lld\n", (long long)elapsed_ms);
        if (ret == 0 || saved_errno != ETIME) {
            snprintf(detail, sizeof detail, "ret=%d errno=%s (%d), expected ETIME", ret,
                     strerror(saved_errno), saved_errno);
            fail("deadline-300ms", detail);
        } else if (elapsed_ms < 270 || elapsed_ms > 400) {
            snprintf(detail, sizeof detail, "elapsed %lld ms outside [270, 400]",
                     (long long)elapsed_ms);
            fail("deadline-300ms", detail);
        } else {
            snprintf(detail, sizeof detail, "ETIME in %lld ms", (long long)elapsed_ms);
            pass("deadline-300ms", detail);
        }

        struct waiter_args w = {.fd = first, .handle = tl, .point = 3};
        pthread_t th;
        if (pthread_create(&th, NULL, timeline_waiter_thread, &w) == 0) {
            while (!w.started)
                sleep_ms(5);
            sleep_ms(60);
            int early_before = w.done;
            expect_ok("signal-point-1", syncobj_timeline_signal(first, tl, 1));
            sleep_ms(60);
            int early_after_1 = w.done;
            uint64_t q1 = 0;
            if (expect_ok("query-point-1", syncobj_query(first, tl, &q1)) == 0 && q1 == 1)
                pass("query-point-1", "1");
            else
                fail("query-point-1", "unexpected value");

            expect_ok("signal-point-2", syncobj_timeline_signal(first, tl, 2));
            sleep_ms(60);
            int early_after_2 = w.done;
            uint64_t q2 = 0;
            if (expect_ok("query-point-2", syncobj_query(first, tl, &q2)) == 0 && q2 == 2)
                pass("query-point-2", "2");
            else
                fail("query-point-2", "unexpected value");

            int64_t sig3_ns = mono_ns();
            expect_ok("signal-point-3", syncobj_timeline_signal(first, tl, 3));
            pthread_join(th, NULL);

            int64_t wake_delay_ms = (w.wake_ns - sig3_ns) / 1000000LL;
            printf(TAG ": wake-delay-ms %lld\n", (long long)wake_delay_ms);
            if (early_before || early_after_1 || early_after_2) {
                fail("timeline-wait-order", "waiter woke before point 3");
            } else if (w.ret != 0) {
                snprintf(detail, sizeof detail, "waiter failed with %s (%d)", strerror(w.err),
                         w.err);
                fail("timeline-wait-order", detail);
            } else if (w.wake_ns < sig3_ns || wake_delay_ms > 50) {
                snprintf(detail, sizeof detail, "wake delay %lld ms outside [0, 50]",
                         (long long)wake_delay_ms);
                fail("timeline-wait-order", detail);
            } else {
                snprintf(detail, sizeof detail, "woke after point 3 in %lld ms",
                         (long long)wake_delay_ms);
                pass("timeline-wait-order", detail);
            }

            uint64_t q3 = 0;
            if (expect_ok("query-point-3", syncobj_query(first, tl, &q3)) == 0 && q3 == 3)
                pass("query-point-3", "3");
            else
                fail("query-point-3", "unexpected value");
        } else {
            fail("timeline-wait-thread", "pthread_create failed");
        }

        uint32_t tl_dst = 0;
        if (expect_ok("create-transfer-dst", syncobj_create(first, 0, &tl_dst)) == 0) {
            struct drm_syncobj_transfer xfer = {
                .src_handle = tl,
                .dst_handle = tl_dst,
                .src_point = 3,
                .dst_point = 7,
                .flags = 0,
            };
            if (expect_ok("transfer-timeline",
                          ioctl(first, DRM_IOCTL_SYNCOBJ_TRANSFER, &xfer)) == 0) {
                uint64_t qd = 0;
                if (expect_ok("query-after-transfer", syncobj_query(first, tl_dst, &qd)) == 0 &&
                    qd == 7)
                    pass("transfer-timeline", "dst point 7 signaled");
                else
                    fail("transfer-timeline", "dst point != 7");
            }
            syncobj_destroy(first, tl_dst);
        }

        struct drm_syncobj_handle h2f = {.handle = tl, .flags = 0, .fd = -1};
        int h2f_ret = ioctl(first, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &h2f);
        switch (check_out("syncobj-handle-to-fd", h2f_ret, h2f.fd >= 0, "exported fd was negative")) {
        case OUT_REFUSED:
        case OUT_MISSING:
            break;
        case OUT_OK: {
            struct drm_syncobj_handle f2h = {.handle = 0, .flags = 0, .fd = h2f.fd};
            int f2h_ret = ioctl(second, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &f2h);
            switch (check_out("syncobj-fd-to-handle", f2h_ret, f2h.handle != 0, "imported handle was 0")) {
            case OUT_REFUSED:
            case OUT_MISSING:
                break;
            case OUT_OK: {
                uint64_t qi = 0;
                if (expect_ok("query-imported-syncobj",
                              syncobj_query(second, f2h.handle, &qi)) == 0 &&
                    qi == 3) {
                    if (expect_ok("signal-imported-syncobj",
                                  syncobj_timeline_signal(second, f2h.handle, 5)) == 0) {
                        uint64_t qo = 0;
                        if (expect_ok("query-original-after-import-signal",
                                      syncobj_query(first, tl, &qo)) == 0 &&
                            qo == 5)
                            pass("syncobj-share", "shared timeline reached point 5");
                        else
                            fail("syncobj-share", "original did not see point 5");
                    }
                } else {
                    fail("syncobj-share", "imported timeline point != 3");
                }
                syncobj_destroy(second, f2h.handle);
                break;
            }
            }
            close(h2f.fd);
            break;
        }
        }
    }

    uint32_t priv = 0;
    if (expect_ok("create-private", syncobj_create(first, 0, &priv)) == 0) {
        uint64_t dummy_point = 0;
        expect_errno("isolation-query", syncobj_query(second, priv, &dummy_point), ENOENT);
        expect_errno("isolation-signal", syncobj_timeline_signal(second, priv, 1), ENOENT);
        expect_errno("isolation-destroy", syncobj_destroy(second, priv), ENOENT);
        struct drm_syncobj_handle bad_h2f = {.handle = priv, .flags = 0, .fd = -1};
        expect_errno("isolation-export", ioctl(second, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &bad_h2f),
                     ENOENT);
        if (expect_ok("isolation-owner-intact", syncobj_query(first, priv, &dummy_point)) == 0 &&
            dummy_point == 0)
            pass("isolation-owner-intact", "first handle kept its syncobj");
        else
            fail("isolation-owner-intact", "first handle lost or altered syncobj");
        syncobj_destroy(first, priv);
    }

    uint32_t sa = 0, sb = 0, sc = 0;
    if (expect_ok("create-sa", syncobj_create(first, 0, &sa)) == 0 &&
        expect_ok("create-sb", syncobj_create(first, 0, &sb)) == 0 &&
        expect_ok("create-sc", syncobj_create(first, 0, &sc)) == 0) {
        struct drm_syncobj_handle exp_a = {
            .handle = sa,
            .flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE |
                     DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE,
            .fd = -1,
            .point = 1,
        };
        struct drm_syncobj_handle exp_b = {
            .handle = sb,
            .flags = DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE |
                     DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_TIMELINE,
            .fd = -1,
            .point = 2,
        };
        int exp_a_ret = ioctl(first, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &exp_a);
        enum out_result exp_a_out = check_out("export-sync-file-a", exp_a_ret, exp_a.fd >= 0, "exported fd was negative");
        int exp_b_ret = ioctl(first, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &exp_b);
        enum out_result exp_b_out = check_out("export-sync-file-b", exp_b_ret, exp_b.fd >= 0, "exported fd was negative");
        if (exp_a_out == OUT_OK && exp_b_out == OUT_OK) {
            uint32_t na = 0, nb = 0;
            int sta = sync_file_status(exp_a.fd, &na);
            int stb = sync_file_status(exp_b.fd, &nb);
            if (sta == 0 && stb == 0 && poll_readable(exp_a.fd, 0) == 0 &&
                poll_readable(exp_b.fd, 0) == 0)
                pass("sync-file-initial-unsignaled", "both active and not readable");
            else {
                snprintf(detail, sizeof detail, "sta=%d stb=%d poll_a=%d poll_b=%d", sta, stb,
                         poll_readable(exp_a.fd, 0), poll_readable(exp_b.fd, 0));
                fail("sync-file-initial-unsignaled", detail);
            }

            struct sync_merge_data merge = {.fd2 = exp_b.fd, .fence = -1};
            strncpy(merge.name, "merged", sizeof merge.name - 1);
            int merge_ret = ioctl(exp_a.fd, SYNC_IOC_MERGE, &merge);
            switch (check_out("sync-file-merge", merge_ret, merge.fence >= 0, "merged fence fd was negative")) {
            case OUT_REFUSED:
            case OUT_MISSING:
                break;
            case OUT_OK: {
                uint32_t nm = 0;
                int stm = sync_file_status(merge.fence, &nm);
                if (stm == 0 && nm == 2 && poll_readable(merge.fence, 0) == 0)
                    pass("merged-unsignaled", "2 fences, active");
                else {
                    snprintf(detail, sizeof detail, "status=%d num_fences=%u poll=%d", stm, nm,
                             poll_readable(merge.fence, 0));
                    fail("merged-unsignaled", detail);
                }

                struct sync_fence_info finfo[2];
                memset(finfo, 0, sizeof finfo);
                struct sync_file_info full = {
                    .num_fences = 2,
                    .sync_fence_info = (uintptr_t)finfo,
                };
                if (expect_ok("sync-file-info-array",
                              ioctl(merge.fence, SYNC_IOC_FILE_INFO, &full)) == 0 &&
                    full.num_fences == 2 && finfo[0].status == 0 && finfo[1].status == 0)
                    pass("sync-file-info-array", "2 active fence infos");
                else
                    fail("sync-file-info-array", "unexpected fence info");

                struct poll_waiter_args pw = {.fd = merge.fence};
                pthread_t pth;
                if (pthread_create(&pth, NULL, poll_waiter_thread, &pw) != 0) {
                    fail("merged-poll-thread", "pthread_create failed");
                } else {
                    expect_ok("signal-sa-1", syncobj_timeline_signal(first, sa, 1));
                    sleep_ms(60);
                    int sta1 = sync_file_status(exp_a.fd, NULL);
                    int stm1 = sync_file_status(merge.fence, NULL);
                    if (sta1 == 1 && poll_readable(exp_a.fd, 0) == 1 && stm1 == 0 &&
                        poll_readable(merge.fence, 0) == 0 && !pw.done)
                        pass("merged-after-first", "first signaled, merged still active");
                    else {
                        snprintf(detail, sizeof detail, "sta1=%d stm1=%d poll_m=%d pw_done=%d", sta1,
                                 stm1, poll_readable(merge.fence, 0), pw.done);
                        fail("merged-after-first", detail);
                    }

                    expect_ok("signal-sb-1", syncobj_timeline_signal(first, sb, 1));
                    sleep_ms(40);
                    if (sync_file_status(merge.fence, NULL) == 0 &&
                        poll_readable(merge.fence, 0) == 0 && !pw.done)
                        pass("merged-before-sb-point-2", "point 1 did not signal point 2");
                    else
                        fail("merged-before-sb-point-2", "signaled too early");

                    int64_t sigb_ns = mono_ns();
                    expect_ok("signal-sb-2", syncobj_timeline_signal(first, sb, 2));
                    pthread_join(pth, NULL);

                    int stm2 = sync_file_status(merge.fence, NULL);
                    int64_t poll_delay_ms = (pw.wake_ns - sigb_ns) / 1000000LL;
                    if (stm2 == 1 && poll_readable(merge.fence, 0) == 1 &&
                        pw.ret == 1 && poll_delay_ms <= 50) {
                        snprintf(detail, sizeof detail, "merged signaled, poll woke in %lld ms",
                                 (long long)poll_delay_ms);
                        pass("merged-after-both", detail);
                    } else {
                        snprintf(detail, sizeof detail, "stm2=%d poll=%d pw_ret=%d delay=%lld", stm2,
                                 poll_readable(merge.fence, 0), pw.ret, (long long)poll_delay_ms);
                        fail("merged-after-both", detail);
                    }

                    struct drm_syncobj_handle imp_sf = {
                        .handle = sc,
                        .flags = DRM_SYNCOBJ_FD_TO_HANDLE_FLAGS_IMPORT_SYNC_FILE,
                        .fd = merge.fence,
                    };
                    if (expect_ok("import-sync-file",
                                  ioctl(first, DRM_IOCTL_SYNCOBJ_FD_TO_HANDLE, &imp_sf)) == 0) {
                        struct drm_syncobj_wait w_sc = {
                            .handles = (uintptr_t)&sc,
                            .timeout_nsec = 0,
                            .count_handles = 1,
                            .flags = DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL,
                        };
                        if (expect_ok("wait-imported-sync-file",
                                      ioctl(first, DRM_IOCTL_SYNCOBJ_WAIT, &w_sc)) == 0)
                            pass("import-sync-file", "imported signaled fence into syncobj");
                    }
                }
                close(merge.fence);
                break;
            }
            }
        }
        if (exp_a.fd >= 0)
            close(exp_a.fd);
        if (exp_b.fd >= 0)
            close(exp_b.fd);
        syncobj_destroy(first, sa);
        syncobj_destroy(first, sb);
        syncobj_destroy(first, sc);
    }

    struct drm_syncobj_create bad_flags = {.flags = 0x80};
    expect_errno("bounds-create-flags", ioctl(first, DRM_IOCTL_SYNCOBJ_CREATE, &bad_flags), EINVAL);
    struct drm_syncobj_wait bad_count = {.handles = 0, .count_handles = 100000};
    expect_errno("bounds-wait-count", ioctl(first, DRM_IOCTL_SYNCOBJ_WAIT, &bad_count), EINVAL);
    struct drm_syncobj_eventfd evfd = {.handle = tl, .flags = 0, .point = 0, .fd = -1};
    expect_errno("eventfd-unsupported", ioctl(first, DRM_IOCTL_SYNCOBJ_EVENTFD, &evfd), EOPNOTSUPP);
    struct drm_mode_fb_cmd2 fb2 = {0};
    expect_errno("refused-getfb2", ioctl(first, DRM_IOCTL_MODE_GETFB2, &fb2), EACCES);
    struct dma_buf_export_sync_file dmabuf_exp = {.flags = 1, .fd = -1};
    expect_errno("dmabuf-export-enotty", ioctl(first, DMA_BUF_IOCTL_EXPORT_SYNC_FILE, &dmabuf_exp),
                 ENOTTY);
    expect_errno("unknown-type-enotty", ioctl(first, IOC_NR(0, 'Z', 1, 0), 0), ENOTTY);

    if (tl)
        syncobj_destroy(first, tl);
    if (bin_unsig)
        syncobj_destroy(first, bin_unsig);
    if (bin_sig)
        syncobj_destroy(first, bin_sig);
    close(second);
    close(first);

    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
