#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

#define TAG "gpu-probe-submit"
#define GATE_JOIN_LIMIT_MS 5000

enum out_result {
    OUT_REFUSED,
    OUT_MISSING,
    OUT_OK,
};

enum gate_phase {
    GATE_ARMED,
    GATE_SIGNALED,
    GATE_SIGNAL_FAILED,
};

#define IOC_NR(dir, type, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | ((unsigned long)(type) << 8) | (nr)))
#define DRM_IOWR(nr, t) IOC_NR(3, 'd', nr, sizeof(t))
#define SYNC_IOWR(nr, t) IOC_NR(3, '>', nr, sizeof(t))

struct drm_virtgpu_execbuffer_syncobj {
    uint32_t handle;
    uint32_t flags;
    uint64_t point;
};

struct drm_virtgpu_execbuffer {
    uint32_t flags;
    uint32_t size;
    uint64_t command;
    uint64_t bo_handles;
    uint32_t num_bo_handles;
    int32_t fence_fd;
    uint32_t ring_idx;
    uint32_t syncobj_stride;
    uint32_t num_in_syncobjs;
    uint32_t num_out_syncobjs;
    uint64_t in_syncobjs;
    uint64_t out_syncobjs;
};

struct drm_virtgpu_3d_wait {
    uint32_t handle;
    uint32_t flags;
};

struct drm_virtgpu_context_set_param {
    uint64_t param;
    uint64_t value;
};

struct drm_virtgpu_context_init {
    uint32_t num_params;
    uint32_t pad;
    uint64_t ctx_set_params;
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

#define DRM_IOCTL_VIRTGPU_EXECBUFFER DRM_IOWR(0x42, struct drm_virtgpu_execbuffer)
#define DRM_IOCTL_VIRTGPU_WAIT DRM_IOWR(0x48, struct drm_virtgpu_3d_wait)
#define DRM_IOCTL_VIRTGPU_CONTEXT_INIT DRM_IOWR(0x4b, struct drm_virtgpu_context_init)
#define DRM_IOCTL_SYNCOBJ_CREATE DRM_IOWR(0xbf, struct drm_syncobj_create)
#define DRM_IOCTL_SYNCOBJ_DESTROY DRM_IOWR(0xc0, struct drm_syncobj_destroy)
#define DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD DRM_IOWR(0xc1, struct drm_syncobj_handle)
#define DRM_IOCTL_SYNCOBJ_WAIT DRM_IOWR(0xc3, struct drm_syncobj_wait)
#define DRM_IOCTL_SYNCOBJ_SIGNAL DRM_IOWR(0xc5, struct drm_syncobj_array)
#define DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT DRM_IOWR(0xca, struct drm_syncobj_timeline_wait)
#define DRM_IOCTL_SYNCOBJ_QUERY DRM_IOWR(0xcb, struct drm_syncobj_timeline_array)
#define SYNC_IOC_MERGE SYNC_IOWR(3, struct sync_merge_data)
#define SYNC_IOC_FILE_INFO SYNC_IOWR(4, struct sync_file_info)

#define DRM_SYNCOBJ_CREATE_SIGNALED (1U << 0)
#define DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE (1U << 0)
#define VIRTGPU_EXECBUF_SYNCOBJ_RESET 0x01

#define VIRTGPU_EXECBUF_FENCE_FD_IN 0x01
#define VIRTGPU_EXECBUF_FENCE_FD_OUT 0x02
#define VIRTGPU_EXECBUF_RING_IDX 0x04

#define VIRTGPU_WAIT_NOWAIT 0x01

#define VIRTGPU_CONTEXT_PARAM_CAPSET_ID 1
#define VIRTGPU_CONTEXT_PARAM_NUM_RINGS 2
#define VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK 3
#define VIRTGPU_CONTEXT_PARAM_DEBUG_NAME 4

#define CAPSET_VENUS 4

#define FENCE_DUP_EXECBUFFER 0x42ULL

extern intptr_t redox_dup_v1(size_t fd, const uint8_t *buf, size_t len);

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

static void expect_errno(const char *check, int ret, int want)
{
    char detail[160];
    if (ret == 0) {
        snprintf(detail, sizeof detail, "accepted; expected %s", strerror(want));
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

static int64_t monotonic_ms(void)
{
    struct timespec ts = {0};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static int64_t monotonic_ns(void)
{
    struct timespec ts = {0};
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000 + ts.tv_nsec;
}

static uint32_t syncobj_create(int fd, uint32_t flags)
{
    struct drm_syncobj_create create = {.flags = flags};
    if (ioctl(fd, DRM_IOCTL_SYNCOBJ_CREATE, &create) != 0)
        return 0;
    return create.handle;
}

static void syncobj_destroy(int fd, uint32_t handle)
{
    struct drm_syncobj_destroy destroy = {.handle = handle};
    ioctl(fd, DRM_IOCTL_SYNCOBJ_DESTROY, &destroy);
}

static int syncobj_export(int fd, uint32_t handle, uint32_t flags)
{
    struct drm_syncobj_handle args = {.handle = handle, .flags = flags, .fd = -1};
    if (ioctl(fd, DRM_IOCTL_SYNCOBJ_HANDLE_TO_FD, &args) != 0)
        return -1;
    return args.fd;
}

static int syncobj_wait(int fd, uint32_t handle, int64_t timeout_ms)
{
    struct drm_syncobj_wait wait = {
        .handles = (uintptr_t)&handle,
        .timeout_nsec = monotonic_ns() + timeout_ms * 1000000,
        .count_handles = 1,
    };
    return ioctl(fd, DRM_IOCTL_SYNCOBJ_WAIT, &wait);
}

static int sync_file_status(int sync_fd, uint32_t *num_fences, uint64_t *timestamp_ns)
{
    struct sync_fence_info fences[4];
    memset(fences, 0, sizeof fences);
    struct sync_file_info info = {.num_fences = 4, .sync_fence_info = (uintptr_t)fences};
    if (ioctl(sync_fd, SYNC_IOC_FILE_INFO, &info) != 0)
        return -1000;
    if (num_fences)
        *num_fences = info.num_fences;
    if (timestamp_ns)
        *timestamp_ns = fences[0].timestamp_ns;
    return info.status;
}

struct blocked_submit {
    int fd;
    int sync_fd;
    int ret;
    int err;
    int out_fd;
    atomic_int done;
};

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

static void *blocked_submit_thread(void *arg)
{
    struct blocked_submit *b = arg;
    struct drm_virtgpu_execbuffer submit = {
        .flags = VIRTGPU_EXECBUF_RING_IDX | VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_FENCE_FD_OUT,
        .ring_idx = 0,
        .fence_fd = b->sync_fd,
    };
    b->ret = ioctl(b->fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &submit);
    b->err = errno;
    b->out_fd = submit.fence_fd;
    atomic_store(&b->done, 1);
    return NULL;
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

static int context_init(int fd, const struct drm_virtgpu_context_set_param *params, uint32_t count)
{
    struct drm_virtgpu_context_init request = {.num_params = count, .ctx_set_params = (uintptr_t)params};
    return ioctl(fd, DRM_IOCTL_VIRTGPU_CONTEXT_INIT, &request);
}

static int dup_with_meta(int fd, uint64_t cmd, uint32_t handle, uint32_t flags)
{
    uint8_t meta[16];
    memcpy(meta, &cmd, 8);
    memcpy(meta + 8, &handle, 4);
    memcpy(meta + 12, &flags, 4);
    intptr_t res = redox_dup_v1((size_t)fd, meta, sizeof meta);
    if (res < 0 && res >= -4095) {
        errno = (int)(-res);
        return -1;
    }
    return (int)res;
}

int gpu_probe_submit(int argc, char **argv)
{
    char render[256], path[300], detail[256];
    (void)argc;
    (void)argv;
    setvbuf(stdout, NULL, _IOLBF, 0);

    static const char ctx_name[] = TAG;
    printf(TAG ": context-name %s\n", ctx_name);

    if (!find_render_name(render, sizeof render)) {
        fail("render-name", "no gpu.* name in /scheme");
        printf(TAG ": FAIL\n");
        return 1;
    }
    printf(TAG ": render %s\n", render);
    pass("render-name", render);
    snprintf(path, sizeof path, "/scheme/%s", render);

    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (expect_ok("open-render", fd) < 0) {
        printf(TAG ": FAIL\n");
        return 1;
    }

    struct drm_virtgpu_execbuffer before_ctx = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .ring_idx = 0,
    };
    expect_errno("submit-before-context", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &before_ctx), EINVAL);

    const struct drm_virtgpu_context_set_param venus_params[] = {
        {VIRTGPU_CONTEXT_PARAM_CAPSET_ID, CAPSET_VENUS},
        {VIRTGPU_CONTEXT_PARAM_NUM_RINGS, 64},
        {VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK, 0},
        {VIRTGPU_CONTEXT_PARAM_DEBUG_NAME, (uintptr_t)ctx_name},
    };
    if (expect_ok("context-init", context_init(fd, venus_params, 4)) == 0)
        pass("context-init", "Venus context with 64 rings");

    struct drm_virtgpu_execbuffer bad_flags = {.flags = 0x100};
    expect_errno("execbuf-unknown-flags", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &bad_flags), EINVAL);

    uint32_t probe_obj = syncobj_create(fd, 0);
    if (!probe_obj)
        fail("syncobj-create", strerror(errno));

    struct drm_virtgpu_execbuffer fence_in_bad = {
        .flags = VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_RING_IDX,
        .fence_fd = -1,
    };
    expect_errno("execbuf-fence-in-bad-fd", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &fence_in_bad), EINVAL);

    int syncobj_fd = syncobj_export(fd, probe_obj, 0);
    if (expect_ok("syncobj-fd", syncobj_fd) >= 0) {
        struct drm_virtgpu_execbuffer fence_in_wrong = {
            .flags = VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_RING_IDX,
            .fence_fd = syncobj_fd,
        };
        expect_errno("execbuf-fence-in-not-sync-file",
                     ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &fence_in_wrong), EINVAL);
        close(syncobj_fd);
    }

    struct drm_virtgpu_execbuffer bad_ring = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .ring_idx = 64,
    };
    expect_errno("execbuf-ring-oob", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &bad_ring), EINVAL);

    struct drm_virtgpu_execbuffer_syncobj syncobj = {.handle = 9999, .flags = 0, .point = 1};
    struct drm_virtgpu_execbuffer bad_stride = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .syncobj_stride = 8,
        .num_out_syncobjs = 1,
        .out_syncobjs = (uintptr_t)&syncobj,
    };
    expect_errno("execbuf-bad-syncobj-stride", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &bad_stride), EINVAL);

    struct drm_virtgpu_execbuffer out_unknown = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .syncobj_stride = 16,
        .num_out_syncobjs = 1,
        .out_syncobjs = (uintptr_t)&syncobj,
    };
    expect_errno("execbuf-out-syncobj-unknown", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &out_unknown), ENOENT);

    struct drm_virtgpu_execbuffer_syncobj out_flagged = {.handle = probe_obj, .flags = 1, .point = 0};
    struct drm_virtgpu_execbuffer out_flags = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .syncobj_stride = 16,
        .num_out_syncobjs = 1,
        .out_syncobjs = (uintptr_t)&out_flagged,
    };
    expect_errno("execbuf-out-syncobj-flags", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &out_flags), EINVAL);

    struct drm_virtgpu_execbuffer_syncobj in_flagged = {.handle = probe_obj, .flags = 2, .point = 0};
    struct drm_virtgpu_execbuffer in_flags = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .syncobj_stride = 16,
        .num_in_syncobjs = 1,
        .in_syncobjs = (uintptr_t)&in_flagged,
    };
    expect_errno("execbuf-in-syncobj-flags", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &in_flags), EINVAL);

    struct drm_virtgpu_execbuffer_syncobj in_empty = {.handle = probe_obj, .flags = 0, .point = 0};
    struct drm_virtgpu_execbuffer in_no_fence = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .syncobj_stride = 16,
        .num_in_syncobjs = 1,
        .in_syncobjs = (uintptr_t)&in_empty,
    };
    expect_errno("execbuf-in-syncobj-no-fence", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &in_no_fence), EINVAL);

    uint8_t dummy_cmd[4] = {0};
    struct drm_virtgpu_execbuffer oversized = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .size = 8 * 1024 * 1024,
        .command = (uintptr_t)dummy_cmd,
    };
    expect_errno("execbuf-oversized", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &oversized), EINVAL);

    struct drm_virtgpu_3d_wait wait_bad_flags = {.handle = 1, .flags = 2};
    expect_errno("wait-bad-flags", ioctl(fd, DRM_IOCTL_VIRTGPU_WAIT, &wait_bad_flags), EINVAL);

    struct drm_virtgpu_3d_wait wait_missing = {.handle = 9999, .flags = 0};
    expect_errno("wait-unknown-handle", ioctl(fd, DRM_IOCTL_VIRTGPU_WAIT, &wait_missing), ENOENT);

    struct drm_virtgpu_3d_wait wait_missing_nowait = {.handle = 9999, .flags = VIRTGPU_WAIT_NOWAIT};
    expect_errno("wait-unknown-nowait", ioctl(fd, DRM_IOCTL_VIRTGPU_WAIT, &wait_missing_nowait), ENOENT);

    expect_errno("dup-bad-fence-handle", dup_with_meta(fd, FENCE_DUP_EXECBUFFER, 9999, 0), ENOENT);
    expect_errno("dup-unknown-fence-command", dup_with_meta(fd, 0x4201, 0, 0), EINVAL);

    int first_fence = -1;
    uint64_t first_stamp = 0;
    struct drm_virtgpu_execbuffer submit = {
        .flags = VIRTGPU_EXECBUF_RING_IDX | VIRTGPU_EXECBUF_FENCE_FD_OUT,
        .ring_idx = 0,
        .fence_fd = -1,
    };
    int64_t t0 = monotonic_ms();
    int submit_ret = ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &submit);
    switch (check_out("submit-ring0", submit_ret, submit.fence_fd >= 0, "fence_fd was negative")) {
    case OUT_REFUSED:
    case OUT_MISSING:
        break;
    case OUT_OK: {
        struct pollfd pfd = {.fd = submit.fence_fd, .events = POLLIN};
        int pret = poll(&pfd, 1, 5000);
        int64_t waited = monotonic_ms() - t0;
        uint32_t count = 0;
        int status = sync_file_status(submit.fence_fd, &count, &first_stamp);
        if (pret == 1 && (pfd.revents & POLLIN) && status == 1 && count == 1 && first_stamp > 0) {
            printf(TAG ": submit-ring0 signaled wait_ms=%lld\n", (long long)waited);
            snprintf(detail, sizeof detail, "sync file of one fence signaled in %lld ms", (long long)waited);
            pass("submit-ring0-poll", detail);
        } else {
            snprintf(detail, sizeof detail, "poll=%d revents=%#x status=%d fences=%u wait_ms=%lld",
                     pret, (unsigned)pfd.revents, status, count, (long long)waited);
            fail("submit-ring0-poll", detail);
        }

        pfd.revents = 0;
        int repoll = poll(&pfd, 1, 0);
        if (repoll == 1 && (pfd.revents & POLLIN))
            pass("fence-stays-readable", "zero-timeout poll after signal");
        else
            fail("fence-stays-readable", "not readable on second poll");

        int dup_fd = dup(submit.fence_fd);
        if (expect_ok("fence-dup", dup_fd) >= 0) {
            struct pollfd dpfd = {.fd = dup_fd, .events = POLLIN};
            int dret = poll(&dpfd, 1, 0);
            if (dret == 1 && (dpfd.revents & POLLIN))
                pass("fence-dup-poll", "duplicated fence descriptor readable");
            else
                fail("fence-dup-poll", "duplicated fence descriptor not readable");
            close(dup_fd);
        }
        first_fence = submit.fence_fd;
        break;
    }
    }

    struct drm_virtgpu_execbuffer no_fence = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .ring_idx = 0,
        .fence_fd = -1,
    };
    if (expect_ok("submit-no-fence-fd", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &no_fence)) == 0) {
        if (no_fence.fence_fd == -1)
            pass("submit-no-fence-fd", "fence_fd unchanged");
        else
            fail("submit-no-fence-fd", "fence_fd unexpectedly modified");
    }

    struct drm_virtgpu_execbuffer submit2 = {
        .flags = VIRTGPU_EXECBUF_RING_IDX | VIRTGPU_EXECBUF_FENCE_FD_OUT,
        .ring_idx = 0,
        .fence_fd = -1,
    };
    int submit2_ret = ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &submit2);
    switch (check_out("submit-second", submit2_ret, submit2.fence_fd >= 0, "fence_fd was negative")) {
    case OUT_REFUSED:
    case OUT_MISSING:
        break;
    case OUT_OK: {
        struct pollfd pfd2 = {.fd = submit2.fence_fd, .events = POLLIN};
        int pret2 = poll(&pfd2, 1, 5000);
        uint64_t stamp2 = 0;
        int status2 = sync_file_status(submit2.fence_fd, NULL, &stamp2);
        if (pret2 == 1 && status2 == 1 && stamp2 >= first_stamp) {
            snprintf(detail, sizeof detail, "signaled %llu ns after the first",
                     (unsigned long long)(stamp2 - first_stamp));
            pass("submit-second-after-first", detail);
        } else {
            snprintf(detail, sizeof detail, "poll=%d status=%d stamp=%llu first=%llu",
                     pret2, status2, (unsigned long long)stamp2, (unsigned long long)first_stamp);
            fail("submit-second-after-first", detail);
        }
        if (first_fence >= 0) {
            struct sync_merge_data merge = {.name = "probe-merge", .fd2 = submit2.fence_fd, .fence = -1};
            int merge_ret = ioctl(first_fence, SYNC_IOC_MERGE, &merge);
            switch (check_out("merge-submission-fences", merge_ret, merge.fence >= 0, "merged fence fd was negative")) {
            case OUT_REFUSED:
            case OUT_MISSING:
                break;
            case OUT_OK: {
                uint32_t count = 0;
                int status = sync_file_status(merge.fence, &count, NULL);
                snprintf(detail, sizeof detail, "status=%d fences=%u", status, count);
                if (status == 1 && count == 2)
                    pass("merge-submission-fences", detail);
                else
                    fail("merge-submission-fences", detail);
                close(merge.fence);
                break;
            }
            }
        }
        close(submit2.fence_fd);
        break;
    }
    }
    if (first_fence >= 0)
        close(first_fence);

    uint32_t bin = syncobj_create(fd, 0);
    uint32_t tl = syncobj_create(fd, 0);
    if (bin && tl) {
        struct drm_virtgpu_execbuffer_syncobj outs[2] = {
            {.handle = bin, .flags = 0, .point = 0},
            {.handle = tl, .flags = 0, .point = 7},
        };
        struct drm_virtgpu_execbuffer out_submit = {
            .flags = VIRTGPU_EXECBUF_RING_IDX,
            .ring_idx = 0,
            .syncobj_stride = 16,
            .num_out_syncobjs = 2,
            .out_syncobjs = (uintptr_t)outs,
        };
        if (expect_ok("submit-out-syncobjs", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &out_submit)) == 0) {
            if (expect_ok("out-binary-wait", syncobj_wait(fd, bin, 5000)) == 0)
                pass("out-binary-wait", "the binary sync object signals with the submission");
            uint64_t point = 7;
            struct drm_syncobj_timeline_wait twait = {
                .handles = (uintptr_t)&tl,
                .points = (uintptr_t)&point,
                .timeout_nsec = monotonic_ns() + 5000000000LL,
                .count_handles = 1,
            };
            if (expect_ok("out-timeline-wait", ioctl(fd, DRM_IOCTL_SYNCOBJ_TIMELINE_WAIT, &twait)) == 0)
                pass("out-timeline-wait", "timeline point 7 signals with the submission");
            uint64_t queried = 0;
            struct drm_syncobj_timeline_array query = {
                .handles = (uintptr_t)&tl,
                .points = (uintptr_t)&queried,
                .count_handles = 1,
            };
            if (expect_ok("out-timeline-query", ioctl(fd, DRM_IOCTL_SYNCOBJ_QUERY, &query)) == 0) {
                snprintf(detail, sizeof detail, "point %llu", (unsigned long long)queried);
                if (queried == 7)
                    pass("out-timeline-query", detail);
                else
                    fail("out-timeline-query", detail);
            }

            struct drm_virtgpu_execbuffer_syncobj in_point = {.handle = tl, .flags = 0, .point = 7};
            struct drm_virtgpu_execbuffer in_submit = {
                .flags = VIRTGPU_EXECBUF_RING_IDX,
                .ring_idx = 0,
                .syncobj_stride = 16,
                .num_in_syncobjs = 1,
                .in_syncobjs = (uintptr_t)&in_point,
            };
            int64_t i0 = monotonic_ms();
            int iret = ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &in_submit);
            int64_t ielapsed = monotonic_ms() - i0;
            if (expect_ok("submit-in-signaled-point", iret) == 0) {
                snprintf(detail, sizeof detail, "returned in %lld ms", (long long)ielapsed);
                if (ielapsed < 1000)
                    pass("submit-in-signaled-point", detail);
                else
                    fail("submit-in-signaled-point", detail);
            }
        }
    } else {
        fail("syncobj-create", "could not create the out sync objects");
    }
    if (bin)
        syncobj_destroy(fd, bin);
    if (tl)
        syncobj_destroy(fd, tl);

    uint32_t resettable = syncobj_create(fd, DRM_SYNCOBJ_CREATE_SIGNALED);
    if (resettable) {
        struct drm_virtgpu_execbuffer_syncobj in_reset = {
            .handle = resettable, .flags = VIRTGPU_EXECBUF_SYNCOBJ_RESET, .point = 0};
        struct drm_virtgpu_execbuffer reset_submit = {
            .flags = VIRTGPU_EXECBUF_RING_IDX,
            .ring_idx = 0,
            .syncobj_stride = 16,
            .num_in_syncobjs = 1,
            .in_syncobjs = (uintptr_t)&in_reset,
        };
        if (expect_ok("submit-in-reset", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &reset_submit)) == 0) {
            if (syncobj_wait(fd, resettable, 0) != 0)
                pass("submit-in-reset", "the in sync object holds no fence after the submission");
            else
                fail("submit-in-reset", "the in sync object is still signaled");
        }
        syncobj_destroy(fd, resettable);
    }

    uint32_t gate = syncobj_create(fd, 0);
    int gate_fd = gate ? syncobj_export(fd, gate, DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE) : -1;
    if (expect_ok("gate-sync-file", gate_fd) >= 0) {
        struct blocked_submit blocked = {.fd = fd, .sync_fd = gate_fd, .out_fd = -1};
        pthread_t thread;
        int64_t b0 = monotonic_ms();
        if (pthread_create(&thread, NULL, blocked_submit_thread, &blocked) != 0) {
            fail("execbuf-fence-in-waits", "pthread_create failed");
        } else {
            usleep(200000);
            int done_early = atomic_load(&blocked.done);
            enum gate_phase phase = GATE_ARMED;
            struct drm_syncobj_array signal = {.handles = (uintptr_t)&gate, .count_handles = 1};
            if (expect_ok("gate-signal", ioctl(fd, DRM_IOCTL_SYNCOBJ_SIGNAL, &signal)) == 0)
                phase = GATE_SIGNALED;
            else
                phase = GATE_SIGNAL_FAILED;
            switch (phase) {
            case GATE_SIGNALED:
                pthread_join(thread, NULL);
                break;
            case GATE_SIGNAL_FAILED: {
                int64_t join_deadline = monotonic_ms() + GATE_JOIN_LIMIT_MS;
                for (int step = 0; step < GATE_JOIN_LIMIT_MS / 10 && !atomic_load(&blocked.done); step++) {
                    if (monotonic_ms() >= join_deadline)
                        break;
                    usleep(10000);
                }
                if (atomic_load(&blocked.done)) {
                    pthread_join(thread, NULL);
                } else {
                    fail("gate-join", "blocked submission thread did not exit after signal failure");
                    printf(TAG ": FAIL\n");
                    exit(1);
                }
                break;
            }
            case GATE_ARMED:
                fail("gate-join", "unexpected gate phase");
                printf(TAG ": FAIL\n");
                exit(1);
            }
            int64_t blocked_ms = monotonic_ms() - b0;
            snprintf(detail, sizeof detail, "early=%d ret=%d errno=%d returned after %lld ms",
                     done_early, blocked.ret, blocked.ret ? blocked.err : 0, (long long)blocked_ms);
            if (!done_early && blocked.ret == 0 && blocked_ms >= 180)
                pass("execbuf-fence-in-waits", detail);
            else
                fail("execbuf-fence-in-waits", detail);
            if (blocked.ret == 0) {
                switch (check_out("execbuf-fence-in-out-fence", 0, blocked.out_fd >= 0, "out_fd was negative")) {
                case OUT_REFUSED:
                case OUT_MISSING:
                    break;
                case OUT_OK: {
                    struct pollfd opfd = {.fd = blocked.out_fd, .events = POLLIN};
                    if (poll(&opfd, 1, 5000) == 1)
                        pass("execbuf-fence-in-out-fence", "the submission behind the gate signaled");
                    else
                        fail("execbuf-fence-in-out-fence", "the submission behind the gate did not signal");
                    close(blocked.out_fd);
                    break;
                }
                }
            }
        }
        close(gate_fd);
    }
    if (gate)
        syncobj_destroy(fd, gate);

    uint32_t idle = syncobj_create(fd, 0);
    int idle_fd = idle ? syncobj_export(fd, idle, DRM_SYNCOBJ_HANDLE_TO_FD_FLAGS_EXPORT_SYNC_FILE) : -1;
    if (expect_ok("unsignaled-sync-file", idle_fd) >= 0) {
        int status = sync_file_status(idle_fd, NULL, NULL);
        if (status == 0)
            pass("unsignaled-state", "status 0");
        else
            fail("unsignaled-state", "unexpected initial status");

        struct pollfd upfd = {.fd = idle_fd, .events = POLLIN};
        int64_t u0 = monotonic_ms();
        int uret = poll(&upfd, 1, 300);
        int64_t elapsed = monotonic_ms() - u0;
        printf(TAG ": timeout-poll ret=%d revents=%#x elapsed_ms=%lld\n",
               uret, (unsigned)upfd.revents, (long long)elapsed);
        snprintf(detail, sizeof detail, "ret=%d revents=%#x elapsed=%lld ms",
                 uret, (unsigned)upfd.revents, (long long)elapsed);
        if (uret == 0 && upfd.revents == 0 && elapsed >= 270 && elapsed <= 400)
            pass("unsignaled-poll-300ms", detail);
        else
            fail("unsignaled-poll-300ms", detail);
        close(idle_fd);
    }
    if (idle)
        syncobj_destroy(fd, idle);
    if (probe_obj)
        syncobj_destroy(fd, probe_obj);

    close(fd);
    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
