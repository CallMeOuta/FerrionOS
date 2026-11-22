#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

#define TAG "gpu-probe-submit"

#define DRM_IOC(dir, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | (0x64UL << 8) | (nr)))
#define DRM_IOWR(nr, type) DRM_IOC(3, nr, sizeof(type))

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

struct virtgpu_fence_state {
    uint32_t signaled;
    int32_t status;
    uint64_t fence_id;
};

#define DRM_IOCTL_VIRTGPU_EXECBUFFER DRM_IOWR(0x42, struct drm_virtgpu_execbuffer)
#define DRM_IOCTL_VIRTGPU_WAIT DRM_IOWR(0x48, struct drm_virtgpu_3d_wait)
#define DRM_IOCTL_VIRTGPU_CONTEXT_INIT DRM_IOWR(0x4b, struct drm_virtgpu_context_init)

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
#define FENCE_DUP_UNSIGNALED 0x4201ULL

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

    struct drm_virtgpu_execbuffer fence_in = {
        .flags = VIRTGPU_EXECBUF_FENCE_FD_IN | VIRTGPU_EXECBUF_RING_IDX,
        .fence_fd = -1,
    };
    expect_errno("execbuf-fence-in-refused", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &fence_in), EOPNOTSUPP);

    struct drm_virtgpu_execbuffer bad_ring = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .ring_idx = 64,
    };
    expect_errno("execbuf-ring-oob", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &bad_ring), EINVAL);

    struct drm_virtgpu_execbuffer_syncobj syncobj = {.handle = 1, .flags = 0, .point = 1};
    struct drm_virtgpu_execbuffer bad_stride = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .syncobj_stride = 8,
        .num_out_syncobjs = 1,
        .out_syncobjs = (uintptr_t)&syncobj,
    };
    expect_errno("execbuf-bad-syncobj-stride", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &bad_stride), EINVAL);

    struct drm_virtgpu_execbuffer syncobj_unimpl = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .syncobj_stride = 16,
        .num_out_syncobjs = 1,
        .out_syncobjs = (uintptr_t)&syncobj,
    };
    expect_errno("execbuf-syncobj-unimpl", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &syncobj_unimpl), EOPNOTSUPP);

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

    uint64_t first_fence_id = 0;
    struct drm_virtgpu_execbuffer submit = {
        .flags = VIRTGPU_EXECBUF_RING_IDX | VIRTGPU_EXECBUF_FENCE_FD_OUT,
        .size = 0,
        .command = 0,
        .ring_idx = 0,
        .syncobj_stride = 0,
        .fence_fd = -1,
    };
    int64_t t0 = monotonic_ms();
    if (expect_ok("submit-ring0", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &submit)) == 0) {
        if (submit.fence_fd < 0) {
            fail("submit-fence-fd", "fence_fd was negative");
        } else {
            struct pollfd pfd = {.fd = submit.fence_fd, .events = POLLIN};
            int pret = poll(&pfd, 1, 5000);
            int64_t waited = monotonic_ms() - t0;
            if (pret == 1 && (pfd.revents & POLLIN)) {
                struct virtgpu_fence_state state = {0};
                ssize_t n = read(submit.fence_fd, &state, sizeof state);
                if (n == (ssize_t)sizeof state && state.signaled == 1 && state.status == 0 && state.fence_id > 0) {
                    first_fence_id = state.fence_id;
                    printf(TAG ": submit-ring0 signaled fence_id=%llu wait_ms=%lld\n",
                           (unsigned long long)state.fence_id, (long long)waited);
                    snprintf(detail, sizeof detail, "fence %llu signaled in %lld ms",
                             (unsigned long long)state.fence_id, (long long)waited);
                    pass("submit-ring0-poll", detail);
                } else {
                    snprintf(detail, sizeof detail, "read=%zd signaled=%u status=%d id=%llu",
                             n, state.signaled, state.status, (unsigned long long)state.fence_id);
                    fail("submit-ring0-state", detail);
                }
            } else {
                snprintf(detail, sizeof detail, "poll=%d revents=%#x errno=%d wait_ms=%lld",
                         pret, (unsigned)pfd.revents, errno, (long long)waited);
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
            close(submit.fence_fd);
        }
    }

    struct drm_virtgpu_execbuffer no_fence = {
        .flags = VIRTGPU_EXECBUF_RING_IDX,
        .size = 0,
        .command = 0,
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
        .size = 0,
        .command = 0,
        .ring_idx = 0,
        .fence_fd = -1,
    };
    if (expect_ok("submit-second", ioctl(fd, DRM_IOCTL_VIRTGPU_EXECBUFFER, &submit2)) == 0 && submit2.fence_fd >= 0) {
        struct pollfd pfd2 = {.fd = submit2.fence_fd, .events = POLLIN};
        int pret2 = poll(&pfd2, 1, 5000);
        struct virtgpu_fence_state state2 = {0};
        ssize_t n2 = read(submit2.fence_fd, &state2, sizeof state2);
        if (pret2 == 1 && (pfd2.revents & POLLIN) && n2 == (ssize_t)sizeof state2 &&
            state2.signaled == 1 && state2.status == 0 && state2.fence_id > first_fence_id) {
            snprintf(detail, sizeof detail, "fence %llu > %llu",
                     (unsigned long long)state2.fence_id, (unsigned long long)first_fence_id);
            pass("submit-second-monotonic", detail);
        } else {
            snprintf(detail, sizeof detail, "poll=%d n=%zd signaled=%u id=%llu prev=%llu",
                     pret2, n2, state2.signaled, (unsigned long long)state2.fence_id,
                     (unsigned long long)first_fence_id);
            fail("submit-second-monotonic", detail);
        }
        close(submit2.fence_fd);
    }

    int unsignaled = dup_with_meta(fd, FENCE_DUP_UNSIGNALED, 0, 0);
    if (expect_ok("unsignaled-fence", unsignaled) >= 0) {
        struct virtgpu_fence_state ustate = {1, 1, 1};
        ssize_t un = read(unsignaled, &ustate, sizeof ustate);
        if (un == (ssize_t)sizeof ustate && ustate.signaled == 0 && ustate.status == 0 && ustate.fence_id == 0)
            pass("unsignaled-state", "signaled=0 status=0 fence_id=0");
        else
            fail("unsignaled-state", "unexpected initial state");

        struct pollfd upfd = {.fd = unsignaled, .events = POLLIN};
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
        close(unsignaled);
    }

    close(fd);
    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
