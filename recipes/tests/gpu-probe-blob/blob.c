#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define TAG "gpu-probe-blob"

#define DRM_IOC(dir, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | (0x64UL << 8) | (nr)))
#define DRM_IOW(nr, type) DRM_IOC(1, nr, sizeof(type))
#define DRM_IOWR(nr, type) DRM_IOC(3, nr, sizeof(type))

struct drm_gem_close {
    uint32_t handle;
    uint32_t pad;
};

struct drm_virtgpu_map {
    uint64_t offset;
    uint32_t handle;
    uint32_t pad;
};

struct drm_virtgpu_getparam {
    uint64_t param;
    uint64_t value;
};

struct drm_virtgpu_resource_info {
    uint32_t bo_handle;
    uint32_t res_handle;
    uint32_t size;
    uint32_t blob_mem;
};

struct drm_virtgpu_resource_create_blob {
    uint32_t blob_mem;
    uint32_t blob_flags;
    uint32_t bo_handle;
    uint32_t res_handle;
    uint64_t size;
    uint32_t pad;
    uint32_t cmd_size;
    uint64_t cmd;
    uint64_t blob_id;
    uint32_t blob_hints;
    uint32_t pad2;
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

#define DRM_IOCTL_GEM_CLOSE DRM_IOW(0x09, struct drm_gem_close)
#define DRM_IOCTL_VIRTGPU_MAP DRM_IOWR(0x41, struct drm_virtgpu_map)
#define DRM_IOCTL_VIRTGPU_GETPARAM DRM_IOWR(0x43, struct drm_virtgpu_getparam)
#define DRM_IOCTL_VIRTGPU_RESOURCE_INFO DRM_IOWR(0x45, struct drm_virtgpu_resource_info)
#define DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB DRM_IOWR(0x4a, struct drm_virtgpu_resource_create_blob)
#define DRM_IOCTL_VIRTGPU_CONTEXT_INIT DRM_IOWR(0x4b, struct drm_virtgpu_context_init)

#define VIRTGPU_PARAM_RESOURCE_BLOB 3
#define VIRTGPU_PARAM_HOST_VISIBLE 4
#define VIRTGPU_PARAM_CONTEXT_INIT 6

#define VIRTGPU_BLOB_MEM_HOST3D 0x0002
#define VIRTGPU_BLOB_FLAG_USE_MAPPABLE 0x0001

#define VIRTGPU_CONTEXT_PARAM_CAPSET_ID 1
#define VIRTGPU_CONTEXT_PARAM_NUM_RINGS 2
#define VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK 3
#define VIRTGPU_CONTEXT_PARAM_DEBUG_NAME 4

#define CAPSET_VENUS 4
#define VENUS_RING_SIZE 131268UL
#define VENUS_RING_ALIGNED 135168UL
#define PAGE_BYTES 4096UL

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
        snprintf(detail, sizeof detail, "failed with %s", strerror(errno));
        fail(check, detail);
    }
    return ret;
}

static void expect_errno(const char *check, int ret, int want)
{
    char detail[160], got_buf[64], want_buf[64];
    int got = errno;
    if (ret == 0) {
        snprintf(detail, sizeof detail, "accepted; expected %s", strerror(want));
        fail(check, detail);
    } else if (got != want) {
        snprintf(got_buf, sizeof got_buf, "%s", strerror(got));
        snprintf(want_buf, sizeof want_buf, "%s", strerror(want));
        snprintf(detail, sizeof detail, "refused with %s (%d); expected %s (%d)", got_buf, got, want_buf, want);
        fail(check, detail);
    } else {
        snprintf(detail, sizeof detail, "refused with %s", strerror(want));
        pass(check, detail);
    }
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

static int getparam(int fd, uint64_t param, int *value)
{
    uint64_t answer = 0;
    struct drm_virtgpu_getparam request = {.param = param, .value = (uintptr_t)&answer};
    int ret = ioctl(fd, DRM_IOCTL_VIRTGPU_GETPARAM, &request);
    *value = (int)answer;
    return ret;
}

static void expect_param(int fd, const char *check, uint64_t param, int want)
{
    char detail[96];
    int value = -1;
    if (expect_ok(check, getparam(fd, param, &value)) < 0)
        return;
    snprintf(detail, sizeof detail, "%d", value);
    if (value == want)
        pass(check, detail);
    else
        fail(check, detail);
}

static int init_venus_context(int fd, const char *name)
{
    const struct drm_virtgpu_context_set_param params[] = {
        {VIRTGPU_CONTEXT_PARAM_CAPSET_ID, CAPSET_VENUS},
        {VIRTGPU_CONTEXT_PARAM_NUM_RINGS, 64},
        {VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK, 0},
        {VIRTGPU_CONTEXT_PARAM_DEBUG_NAME, (uintptr_t)name},
    };
    struct drm_virtgpu_context_init req = {
        .num_params = 4,
        .pad = 0,
        .ctx_set_params = (uintptr_t)params,
    };
    return ioctl(fd, DRM_IOCTL_VIRTGPU_CONTEXT_INIT, &req);
}

static uint32_t pattern_word(uint32_t seed, size_t index)
{
    return (seed * 2654435761U) ^ (uint32_t)(index * 0x9e3779b1U + 0x517cc1b7U);
}

int gpu_probe_blob(int argc, char **argv)
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

    int fd = open(path, O_RDWR | O_CLOEXEC);
    if (expect_ok("open-render", fd) < 0) {
        printf(TAG ": FAIL\n");
        return 1;
    }

    expect_param(fd, "param-resource-blob", VIRTGPU_PARAM_RESOURCE_BLOB, 1);
    expect_param(fd, "param-host-visible", VIRTGPU_PARAM_HOST_VISIBLE, 1);
    expect_param(fd, "param-context-init", VIRTGPU_PARAM_CONTEXT_INIT, 1);

    printf(TAG ": context-name %s\n", TAG);
    if (expect_ok("context-init", init_venus_context(fd, TAG)) < 0) {
        close(fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    pass("context-init", "Venus context created");

    struct drm_virtgpu_resource_create_blob bad_zero = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = 0,
    };
    expect_errno("create-size-zero", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &bad_zero), EINVAL);

    struct drm_virtgpu_resource_create_blob bad_mem = {
        .blob_mem = 99,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = PAGE_BYTES,
    };
    expect_errno("create-bad-mem", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &bad_mem), EINVAL);

    struct drm_virtgpu_resource_create_blob bad_flags = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = 0x8000,
        .size = PAGE_BYTES,
    };
    expect_errno("create-bad-flags", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &bad_flags), EINVAL);

    struct drm_virtgpu_resource_create_blob bad_pad = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = PAGE_BYTES,
        .pad = 1,
    };
    expect_errno("create-bad-pad", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &bad_pad), EINVAL);

    struct drm_virtgpu_map bad_map = {.handle = 0xdead};
    expect_errno("map-unknown", ioctl(fd, DRM_IOCTL_VIRTGPU_MAP, &bad_map), ENOENT);

    struct drm_virtgpu_resource_info bad_info = {.bo_handle = 0xdead};
    expect_errno("info-unknown", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, &bad_info), ENOENT);

    struct drm_virtgpu_resource_create_blob ring = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = VENUS_RING_SIZE,
        .blob_id = 0,
    };
    if (expect_ok("create-ring-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &ring)) == 0) {
        snprintf(detail, sizeof detail, "bo=%u res=%u", ring.bo_handle, ring.res_handle);
        if (ring.bo_handle != 0 && ring.res_handle != 0)
            pass("create-ring-blob", detail);
        else
            fail("create-ring-blob", detail);
        printf(TAG ": blob bo=%u res=%u size=%lu\n", ring.bo_handle, ring.res_handle, (unsigned long)VENUS_RING_SIZE);

        struct drm_virtgpu_resource_info info = {.bo_handle = ring.bo_handle};
        if (expect_ok("info-ring-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_INFO, &info)) == 0) {
            snprintf(detail, sizeof detail, "res=%u size=%u mem=%u", info.res_handle, info.size, info.blob_mem);
            if (info.res_handle == ring.res_handle && info.size == VENUS_RING_ALIGNED &&
                info.blob_mem == VIRTGPU_BLOB_MEM_HOST3D)
                pass("info-ring-blob", detail);
            else
                fail("info-ring-blob", detail);
        }

        struct drm_virtgpu_map map_req = {.handle = ring.bo_handle};
        if (expect_ok("map-ring-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_MAP, &map_req)) == 0) {
            snprintf(detail, sizeof detail, "offset=0x%llx", (unsigned long long)map_req.offset);
            if (map_req.offset != 0 && (map_req.offset & (PAGE_BYTES - 1)) == 0 && (map_req.offset >> 63) == 0)
                pass("map-ring-blob", detail);
            else
                fail("map-ring-blob", detail);

            void *bad_over = mmap(NULL, VENUS_RING_ALIGNED + PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                  (off_t)map_req.offset);
            if (bad_over == MAP_FAILED) {
                pass("mmap-past-end", strerror(errno));
            } else {
                munmap(bad_over, VENUS_RING_ALIGNED + PAGE_BYTES);
                fail("mmap-past-end", "accepted mapping past blob size");
            }

            void *bad_at_end =
                mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)(map_req.offset + VENUS_RING_ALIGNED));
            if (bad_at_end == MAP_FAILED) {
                pass("mmap-at-end", strerror(errno));
            } else {
                munmap(bad_at_end, PAGE_BYTES);
                fail("mmap-at-end", "accepted mapping at blob end");
            }

            void *bad_slot = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)(0x1234ULL << 40));
            if (bad_slot == MAP_FAILED) {
                pass("mmap-unknown-slot", strerror(errno));
            } else {
                munmap(bad_slot, PAGE_BYTES);
                fail("mmap-unknown-slot", "accepted mapping for unknown slot");
            }

            void *ptr1 = mmap(NULL, VENUS_RING_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)map_req.offset);
            if (ptr1 == MAP_FAILED) {
                fail("mmap-ring-blob", strerror(errno));
            } else {
                volatile uint32_t *words = (volatile uint32_t *)ptr1;
                size_t count = VENUS_RING_ALIGNED / sizeof(uint32_t);
                for (size_t i = 0; i < count; i++)
                    words[i] = pattern_word(1, i);
                __sync_synchronize();
                size_t mismatches = 0;
                for (size_t i = 0; i < count; i++) {
                    if (words[i] != pattern_word(1, i))
                        mismatches++;
                }
                snprintf(detail, sizeof detail, "words=%zu mismatches=%zu", count, mismatches);
                if (mismatches == 0)
                    pass("ring-pattern-readback", detail);
                else
                    fail("ring-pattern-readback", detail);

                void *ptr2 = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                                  (off_t)(map_req.offset + 32 * PAGE_BYTES));
                if (ptr2 == MAP_FAILED) {
                    fail("mmap-ring-second", strerror(errno));
                } else {
                    volatile uint32_t *tail = (volatile uint32_t *)ptr2;
                    uint32_t tail_got = tail[0];
                    uint32_t tail_want = pattern_word(1, (32 * PAGE_BYTES) / sizeof(uint32_t));
                    tail[1] = 0xCAFEBABEU;
                    __sync_synchronize();
                    uint32_t seen_in_1 = words[(32 * PAGE_BYTES) / sizeof(uint32_t) + 1];
                    if (tail_got == tail_want && seen_in_1 == 0xCAFEBABEU)
                        pass("ring-shared-coherence", "tail page coherent across two mappings");
                    else {
                        snprintf(detail, sizeof detail, "got=0x%x want=0x%x seen=0x%x", tail_got, tail_want, seen_in_1);
                        fail("ring-shared-coherence", detail);
                    }
                    if (expect_ok("munmap-ring-second", munmap(ptr2, PAGE_BYTES)) == 0)
                        pass("munmap-ring-second", "unmapped tail view");
                    if (words[0] == pattern_word(1, 0) && words[(32 * PAGE_BYTES) / sizeof(uint32_t) + 1] == 0xCAFEBABEU)
                        pass("ring-alive-after-second-unmap", "first mapping intact");
                    else
                        fail("ring-alive-after-second-unmap", "first mapping corrupted");
                }

                if (expect_ok("munmap-ring-first", munmap(ptr1, VENUS_RING_SIZE)) == 0)
                    pass("munmap-ring-first", "unmapped ring blob");
            }

            void *ptr3 = mmap(NULL, VENUS_RING_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)map_req.offset);
            if (ptr3 == MAP_FAILED) {
                fail("remap-ring-blob", strerror(errno));
            } else {
                volatile uint32_t *words = (volatile uint32_t *)ptr3;
                words[0] = 0x11223344U;
                words[(VENUS_RING_ALIGNED / sizeof(uint32_t)) - 1] = 0x55667788U;
                __sync_synchronize();
                if (words[0] == 0x11223344U && words[(VENUS_RING_ALIGNED / sizeof(uint32_t)) - 1] == 0x55667788U)
                    pass("remap-ring-readback", "remap after unmap succeeded");
                else
                    fail("remap-ring-readback", "remap readback mismatch");
                if (expect_ok("munmap-ring-remap", munmap(ptr3, VENUS_RING_SIZE)) == 0)
                    pass("munmap-ring-remap", "unmapped remapped blob");
            }

            struct drm_virtgpu_resource_create_blob swap_blob = {
                .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
                .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
                .size = 65536,
                .blob_id = 0,
            };
            if (expect_ok("create-swap-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &swap_blob)) == 0) {
                struct drm_virtgpu_map swap_map = {.handle = swap_blob.bo_handle};
                if (expect_ok("map-swap-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_MAP, &swap_map)) == 0) {
                    void *swap_ptr = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)swap_map.offset);
                    if (swap_ptr == MAP_FAILED) {
                        fail("mmap-swap-blob", strerror(errno));
                    } else {
                        volatile uint32_t *swords = (volatile uint32_t *)swap_ptr;
                        uint32_t initial_swap = swords[0];
                        swords[0] = 0xB1B2B3B4U;
                        __sync_synchronize();
                        munmap(swap_ptr, 65536);

                        void *ptr4 = mmap(NULL, VENUS_RING_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)map_req.offset);
                        if (ptr4 == MAP_FAILED) {
                            fail("host-backing-swap", strerror(errno));
                        } else {
                            volatile uint32_t *rwords = (volatile uint32_t *)ptr4;
                            uint32_t ring_back = rwords[0];
                            uint32_t ring_tail = rwords[(VENUS_RING_ALIGNED / sizeof(uint32_t)) - 1];
                            munmap(ptr4, VENUS_RING_SIZE);
                            if (initial_swap != 0x11223344U && ring_back == 0x11223344U && ring_tail == 0x55667788U)
                                pass("host-backing-swap", "distinct host blobs persisted across window reuse");
                            else {
                                snprintf(detail, sizeof detail, "swap0=0x%x ring0=0x%x tail=0x%x", initial_swap, ring_back, ring_tail);
                                fail("host-backing-swap", detail);
                            }
                        }
                    }
                }
                struct drm_gem_close close_swap = {.handle = swap_blob.bo_handle};
                ioctl(fd, DRM_IOCTL_GEM_CLOSE, &close_swap);
            }

            struct drm_gem_close close_req = {.handle = ring.bo_handle};
            if (expect_ok("gem-close-ring", ioctl(fd, DRM_IOCTL_GEM_CLOSE, &close_req)) == 0)
                pass("gem-close-ring", "closed ring blob");
            expect_errno("gem-close-ring-again", ioctl(fd, DRM_IOCTL_GEM_CLOSE, &close_req), EINVAL);
            struct drm_virtgpu_map closed_map = {.handle = ring.bo_handle};
            expect_errno("map-closed-ring", ioctl(fd, DRM_IOCTL_VIRTGPU_MAP, &closed_map), ENOENT);
            void *after_close = mmap(NULL, PAGE_BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)map_req.offset);
            if (after_close == MAP_FAILED) {
                pass("mmap-after-close", strerror(errno));
            } else {
                munmap(after_close, PAGE_BYTES);
                fail("mmap-after-close", "accepted mmap after GEM_CLOSE");
            }
        }
    }

    struct drm_virtgpu_resource_create_blob hold = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = 65536,
        .blob_id = 0,
    };
    if (expect_ok("create-hold-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &hold)) == 0) {
        struct drm_virtgpu_map hold_map = {.handle = hold.bo_handle};
        if (expect_ok("map-hold-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_MAP, &hold_map)) == 0) {
            void *ptr = mmap(NULL, 65536, PROT_READ | PROT_WRITE, MAP_SHARED, fd, (off_t)hold_map.offset);
            if (ptr == MAP_FAILED) {
                fail("mmap-hold-blob", strerror(errno));
            } else {
                volatile uint32_t *words = (volatile uint32_t *)ptr;
                words[0] = 0xDEADBEEFU;
                words[16383] = 0x01020304U;
                struct drm_gem_close close_hold = {.handle = hold.bo_handle};
                if (expect_ok("close-while-mapped", ioctl(fd, DRM_IOCTL_GEM_CLOSE, &close_hold)) == 0)
                    pass("close-while-mapped", "GEM_CLOSE while mapped succeeded");
                __sync_synchronize();
                if (words[0] == 0xDEADBEEFU && words[16383] == 0x01020304U)
                    pass("read-after-close-while-mapped", "mapping stayed valid until munmap");
                else
                    fail("read-after-close-while-mapped", "data mismatch");
                if (expect_ok("munmap-after-close", munmap(ptr, 65536)) == 0)
                    pass("munmap-after-close", "released closed mapping");
            }
        }
    }

    struct drm_virtgpu_resource_create_blob leftover = {
        .blob_mem = VIRTGPU_BLOB_MEM_HOST3D,
        .blob_flags = VIRTGPU_BLOB_FLAG_USE_MAPPABLE,
        .size = 65536,
        .blob_id = 0,
    };
    if (expect_ok("create-leftover-blob", ioctl(fd, DRM_IOCTL_VIRTGPU_RESOURCE_CREATE_BLOB, &leftover)) == 0) {
        int fd2 = open(path, O_RDWR | O_CLOEXEC);
        if (expect_ok("open-second", fd2) >= 0) {
            if (expect_ok("context-second", init_venus_context(fd2, TAG "-2")) == 0) {
                struct drm_virtgpu_map foreign_map = {.handle = leftover.bo_handle};
                expect_errno("foreign-map", ioctl(fd2, DRM_IOCTL_VIRTGPU_MAP, &foreign_map), ENOENT);
                struct drm_gem_close foreign_close = {.handle = leftover.bo_handle};
                expect_errno("foreign-close", ioctl(fd2, DRM_IOCTL_GEM_CLOSE, &foreign_close), EINVAL);
            }
            close(fd2);
        }
    }

    close(fd);
    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
