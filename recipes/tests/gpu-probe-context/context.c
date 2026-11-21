#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define TAG "gpu-probe-context"

#define DRM_IOC(dir, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | (0x64UL << 8) | (nr)))
#define DRM_IOW(nr, type) DRM_IOC(1, nr, sizeof(type))
#define DRM_IOWR(nr, type) DRM_IOC(3, nr, sizeof(type))

struct drm_version {
    int version_major;
    int version_minor;
    int version_patchlevel;
    size_t name_len;
    char *name;
    size_t date_len;
    char *date;
    size_t desc_len;
    char *desc;
};

struct drm_gem_close {
    uint32_t handle;
    uint32_t pad;
};

struct drm_set_client_cap {
    uint64_t capability;
    uint64_t value;
};

struct drm_mode_card_res {
    uint64_t fb_id_ptr;
    uint64_t crtc_id_ptr;
    uint64_t connector_id_ptr;
    uint64_t encoder_id_ptr;
    uint32_t count_fbs;
    uint32_t count_crtcs;
    uint32_t count_connectors;
    uint32_t count_encoders;
    uint32_t min_width;
    uint32_t max_width;
    uint32_t min_height;
    uint32_t max_height;
};

struct drm_mode_modeinfo {
    uint32_t clock;
    uint16_t hdisplay, hsync_start, hsync_end, htotal, hskew;
    uint16_t vdisplay, vsync_start, vsync_end, vtotal, vscan;
    uint32_t vrefresh;
    uint32_t flags;
    uint32_t type;
    char name[32];
};

struct drm_mode_crtc {
    uint64_t set_connectors_ptr;
    uint32_t count_connectors;
    uint32_t crtc_id;
    uint32_t fb_id;
    uint32_t x;
    uint32_t y;
    uint32_t gamma_size;
    uint32_t mode_valid;
    struct drm_mode_modeinfo mode;
};

struct drm_mode_cursor {
    uint32_t flags;
    uint32_t crtc_id;
    int32_t x;
    int32_t y;
    uint32_t width;
    uint32_t height;
    uint32_t handle;
};

struct drm_mode_create_dumb {
    uint32_t height;
    uint32_t width;
    uint32_t bpp;
    uint32_t flags;
    uint32_t handle;
    uint32_t pitch;
    uint64_t size;
};

struct drm_virtgpu_getparam {
    uint64_t param;
    uint64_t value;
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

struct drm_virtgpu_get_caps {
    uint32_t cap_set_id;
    uint32_t cap_set_ver;
    uint64_t addr;
    uint32_t size;
    uint32_t pad;
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

#define DRM_IOCTL_VERSION DRM_IOWR(0x00, struct drm_version)
#define DRM_IOCTL_GEM_CLOSE DRM_IOW(0x09, struct drm_gem_close)
#define DRM_IOCTL_SET_CLIENT_CAP DRM_IOW(0x0d, struct drm_set_client_cap)
#define DRM_IOCTL_MODE_GETRESOURCES DRM_IOWR(0xa0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_SETCRTC DRM_IOWR(0xa2, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_CURSOR DRM_IOWR(0xa3, struct drm_mode_cursor)
#define DRM_IOCTL_MODE_CREATE_DUMB DRM_IOWR(0xb2, struct drm_mode_create_dumb)
#define DRM_IOCTL_VIRTGPU_EXECBUFFER DRM_IOWR(0x42, struct drm_virtgpu_execbuffer)
#define DRM_IOCTL_VIRTGPU_GETPARAM DRM_IOWR(0x43, struct drm_virtgpu_getparam)
#define DRM_IOCTL_VIRTGPU_GET_CAPS DRM_IOWR(0x49, struct drm_virtgpu_get_caps)
#define DRM_IOCTL_VIRTGPU_CONTEXT_INIT DRM_IOWR(0x4b, struct drm_virtgpu_context_init)

#define VIRTGPU_PARAM_3D_FEATURES 1
#define VIRTGPU_PARAM_CAPSET_QUERY_FIX 2
#define VIRTGPU_PARAM_RESOURCE_BLOB 3
#define VIRTGPU_PARAM_HOST_VISIBLE 4
#define VIRTGPU_PARAM_CROSS_DEVICE 5
#define VIRTGPU_PARAM_CONTEXT_INIT 6
#define VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs 7
#define VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME 8
#define VIRTGPU_PARAM_BLOB_ALIGNMENT 9

#define VIRTGPU_CONTEXT_PARAM_CAPSET_ID 1
#define VIRTGPU_CONTEXT_PARAM_NUM_RINGS 2
#define VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK 3
#define VIRTGPU_CONTEXT_PARAM_DEBUG_NAME 4

#define CAPSET_VENUS 4
#define VENUS_CAPSET_SIZE 160

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
    char detail[160];
    if (ret == 0) {
        snprintf(detail, sizeof detail, "accepted; expected %s", strerror(want));
        fail(check, detail);
    } else if (errno != want) {
        snprintf(detail, sizeof detail, "refused with %s; expected %s", strerror(errno), strerror(want));
        fail(check, detail);
    } else {
        snprintf(detail, sizeof detail, "refused with %s", strerror(want));
        pass(check, detail);
    }
}

static const uint32_t sha256_k[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

static uint32_t rotr(uint32_t x, int n)
{
    return (x >> n) | (x << (32 - n));
}

static void sha256_block(uint32_t h[8], const uint8_t block[64])
{
    uint32_t w[64];
    for (int i = 0; i < 16; i++)
        w[i] = (uint32_t)block[4 * i] << 24 | (uint32_t)block[4 * i + 1] << 16 |
               (uint32_t)block[4 * i + 2] << 8 | block[4 * i + 3];
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5], g = h[6], k = h[7];
    for (int i = 0; i < 64; i++) {
        uint32_t t1 = k + (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) + ((e & f) ^ (~e & g)) + sha256_k[i] + w[i];
        uint32_t t2 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) + ((a & b) ^ (a & c) ^ (b & c));
        k = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }
    h[0] += a;
    h[1] += b;
    h[2] += c;
    h[3] += d;
    h[4] += e;
    h[5] += f;
    h[6] += g;
    h[7] += k;
}

static void sha256_hex(const uint8_t *data, size_t len, char out[65])
{
    uint32_t h[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                     0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    uint8_t block[64];
    size_t done = 0;
    for (; len - done >= 64; done += 64)
        sha256_block(h, data + done);
    size_t rest = len - done;
    memset(block, 0, sizeof block);
    memcpy(block, data + done, rest);
    block[rest] = 0x80;
    if (rest >= 56) {
        sha256_block(h, block);
        memset(block, 0, sizeof block);
    }
    uint64_t bits = (uint64_t)len * 8;
    for (int i = 0; i < 8; i++)
        block[63 - i] = (uint8_t)(bits >> (8 * i));
    sha256_block(h, block);
    for (int i = 0; i < 8; i++)
        snprintf(out + 8 * i, 9, "%08x", h[i]);
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

static int get_caps(int fd, uint32_t id, uint32_t version, void *buf, uint32_t size)
{
    struct drm_virtgpu_get_caps request = {
        .cap_set_id = id, .cap_set_ver = version, .addr = (uintptr_t)buf, .size = size};
    int ret = ioctl(fd, DRM_IOCTL_VIRTGPU_GET_CAPS, &request);
    if (ret == 0 && request.size != size) {
        char detail[96];
        snprintf(detail, sizeof detail, "capset %u: size became %u", id, request.size);
        fail("caps-size-kept", detail);
    }
    return ret;
}

static int context_init(int fd, const struct drm_virtgpu_context_set_param *params, uint32_t count)
{
    struct drm_virtgpu_context_init request = {.num_params = count, .ctx_set_params = (uintptr_t)params};
    return ioctl(fd, DRM_IOCTL_VIRTGPU_CONTEXT_INIT, &request);
}

static void check_version(int fd, const char *which)
{
    char check[64], detail[160];
    struct drm_version version = {0};
    snprintf(check, sizeof check, "version-%s", which);
    if (expect_ok(check, ioctl(fd, DRM_IOCTL_VERSION, &version)) < 0)
        return;
    if (version.name_len > 64 || version.desc_len > 256 || version.date_len > 64) {
        snprintf(detail, sizeof detail, "lengths %zu, %zu, %zu", version.name_len, version.date_len,
                 version.desc_len);
        fail(check, detail);
        return;
    }
    char name[65] = {0}, date[65] = {0}, desc[257] = {0};
    version.name = name;
    version.date = date;
    version.desc = desc;
    if (expect_ok(check, ioctl(fd, DRM_IOCTL_VERSION, &version)) < 0)
        return;
    snprintf(detail, sizeof detail, "%s %d.%d.%d, \"%s\"", name, version.version_major,
             version.version_minor, version.version_patchlevel, desc);
    if (strcmp(name, "virtio_gpu") == 0 && version.version_major == 0)
        pass(check, detail);
    else
        fail(check, detail);
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

static void check_display_refused(int fd, const char *which)
{
    char check[64];
    struct drm_mode_card_res res = {0};
    struct drm_mode_crtc crtc = {0};
    struct drm_mode_cursor cursor = {.flags = 1};
    struct drm_mode_create_dumb dumb = {.height = 32, .width = 32, .bpp = 32};
    struct drm_set_client_cap cap = {.capability = 2, .value = 1};

    snprintf(check, sizeof check, "refused-get-resources-%s", which);
    expect_errno(check, ioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res), EACCES);
    snprintf(check, sizeof check, "refused-set-crtc-%s", which);
    expect_errno(check, ioctl(fd, DRM_IOCTL_MODE_SETCRTC, &crtc), EACCES);
    snprintf(check, sizeof check, "refused-cursor-%s", which);
    expect_errno(check, ioctl(fd, DRM_IOCTL_MODE_CURSOR, &cursor), EACCES);
    snprintf(check, sizeof check, "refused-create-dumb-%s", which);
    expect_errno(check, ioctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &dumb), EACCES);
    snprintf(check, sizeof check, "refused-client-cap-%s", which);
    expect_errno(check, ioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &cap), EACCES);
}

static void check_gem_close(int fd, const char *which)
{
    char check[64];
    for (uint32_t handle = 0; handle < 4; handle++) {
        struct drm_gem_close close_request = {.handle = handle};
        snprintf(check, sizeof check, "gem-close-%u-%s", handle, which);
        expect_errno(check, ioctl(fd, DRM_IOCTL_GEM_CLOSE, &close_request), EINVAL);
    }
    struct drm_gem_close padded = {.handle = 1, .pad = 1};
    snprintf(check, sizeof check, "gem-close-pad-%s", which);
    expect_errno(check, ioctl(fd, DRM_IOCTL_GEM_CLOSE, &padded), EINVAL);
}

int gpu_probe_context(int argc, char **argv)
{
    char render[256], path[300], detail[512];
    (void)argc;
    setvbuf(stdout, NULL, _IOLBF, 0);

    const char *self = strrchr(argv[0], '/') ? strrchr(argv[0], '/') + 1 : argv[0];
    printf(TAG ": context-name %.63s\n", self);

    if (!find_render_name(render, sizeof render)) {
        fail("render-name", "no gpu.* name in /scheme");
        printf(TAG ": FAIL\n");
        return 1;
    }
    printf(TAG ": render %s\n", render);
    pass("render-name", render);
    snprintf(path, sizeof path, "/scheme/%s", render);

    int first = open(path, O_RDWR | O_CLOEXEC);
    if (expect_ok("open-first", first) < 0) {
        printf(TAG ": FAIL\n");
        return 1;
    }
    check_version(first, "first");

    expect_param(first, "param-3d-features", VIRTGPU_PARAM_3D_FEATURES, 1);
    expect_param(first, "param-capset-query-fix", VIRTGPU_PARAM_CAPSET_QUERY_FIX, 1);
    expect_param(first, "param-resource-blob", VIRTGPU_PARAM_RESOURCE_BLOB, 1);
    expect_param(first, "param-context-init", VIRTGPU_PARAM_CONTEXT_INIT, 1);
    expect_param(first, "param-explicit-debug-name", VIRTGPU_PARAM_EXPLICIT_DEBUG_NAME, 1);
    expect_param(first, "param-host-visible", VIRTGPU_PARAM_HOST_VISIBLE, 0);
    expect_param(first, "param-cross-device", VIRTGPU_PARAM_CROSS_DEVICE, 0);
    int value;
    expect_errno("param-blob-alignment", getparam(first, VIRTGPU_PARAM_BLOB_ALIGNMENT, &value), ENOENT);
    expect_errno("param-unknown", getparam(first, 100, &value), EINVAL);

    int mask = 0;
    if (expect_ok("param-capset-ids", getparam(first, VIRTGPU_PARAM_SUPPORTED_CAPSET_IDs, &mask)) == 0) {
        char list[128] = {0};
        for (int id = 0; id < 32; id++) {
            if (!(mask & (1 << id)))
                continue;
            uint8_t buf[16];
            snprintf(detail, sizeof detail, "caps-%d", id);
            if (expect_ok(detail, get_caps(first, (uint32_t)id, 0, buf, sizeof buf)) == 0)
                snprintf(list + strlen(list), sizeof list - strlen(list), "%s%d", list[0] ? "," : "", id);
        }
        printf(TAG ": capsets %s\n", list);
        snprintf(detail, sizeof detail, "mask %#x: %s", (unsigned)mask, list);
        if ((mask & 0x16) == 0x16)
            pass("capsets-virgl-venus", detail);
        else
            fail("capsets-virgl-venus", detail);
    }

    uint8_t venus[VENUS_CAPSET_SIZE];
    memset(venus, 0xAA, sizeof venus);
    if (expect_ok("caps-venus", get_caps(first, CAPSET_VENUS, 0, venus, sizeof venus)) == 0) {
        char hash[65], hex[2 * VENUS_CAPSET_SIZE + 1];
        sha256_hex(venus, sizeof venus, hash);
        for (size_t i = 0; i < sizeof venus; i++)
            snprintf(hex + 2 * i, 3, "%02x", venus[i]);
        printf(TAG ": venus-capset size %d sha256 %s\n", VENUS_CAPSET_SIZE, hash);
        printf(TAG ": venus-capset bytes %s\n", hex);
        uint8_t larger[4096];
        memset(larger, 0xAA, sizeof larger);
        if (expect_ok("caps-venus-larger", get_caps(first, CAPSET_VENUS, 0, larger, sizeof larger)) == 0) {
            if (memcmp(larger, venus, sizeof venus) == 0)
                pass("caps-venus-larger", "same 160 bytes first");
            else
                fail("caps-venus-larger", "different bytes");
        }
    }
    uint8_t scratch[16];
    expect_errno("caps-size-zero", get_caps(first, CAPSET_VENUS, 0, scratch, 0), EINVAL);
    expect_errno("caps-version-above", get_caps(first, CAPSET_VENUS, 1, scratch, sizeof scratch), EINVAL);
    expect_errno("caps-not-offered", get_caps(first, 3, 0, scratch, sizeof scratch), EINVAL);
    expect_errno("caps-beyond-ids", get_caps(first, 99, 0, scratch, sizeof scratch), EINVAL);

    const struct drm_virtgpu_context_set_param venus_params[] = {
        {VIRTGPU_CONTEXT_PARAM_CAPSET_ID, CAPSET_VENUS},
        {VIRTGPU_CONTEXT_PARAM_NUM_RINGS, 64},
        {VIRTGPU_CONTEXT_PARAM_POLL_RINGS_MASK, 0},
    };
    if (expect_ok("context-first", context_init(first, venus_params, 3)) == 0)
        pass("context-first", "Venus context, 64 rings, named after the program");
    expect_errno("context-first-again", context_init(first, venus_params, 3), EEXIST);

    int second = open(path, O_RDWR | O_CLOEXEC);
    if (expect_ok("open-second", second) < 0) {
        printf(TAG ": FAIL\n");
        return 1;
    }
    check_version(second, "second");

    static const char second_name[] = TAG "-2";
    static const char other_name[] = "other";
    const struct drm_virtgpu_context_set_param too_many[] = {
        {1, 4}, {2, 1}, {3, 0}, {4, (uintptr_t)second_name}, {2, 1}};
    const struct drm_virtgpu_context_set_param rings_over[] = {{2, 65}};
    const struct drm_virtgpu_context_set_param capset_absent[] = {{1, 3}};
    const struct drm_virtgpu_context_set_param capset_over[] = {{1, 64}};
    const struct drm_virtgpu_context_set_param capset_twice[] = {{1, 4}, {1, 4}};
    const struct drm_virtgpu_context_set_param mask_set[] = {{2, 4}, {3, 1}};
    const struct drm_virtgpu_context_set_param unknown[] = {{5, 0}};
    const struct drm_virtgpu_context_set_param named_twice[] = {
        {4, (uintptr_t)second_name}, {4, (uintptr_t)other_name}};
    expect_errno("context-too-many", context_init(second, too_many, 5), EINVAL);
    expect_errno("context-rings-over", context_init(second, rings_over, 1), EINVAL);
    expect_errno("context-capset-absent", context_init(second, capset_absent, 1), EINVAL);
    expect_errno("context-capset-over", context_init(second, capset_over, 1), EINVAL);
    expect_errno("context-capset-twice", context_init(second, capset_twice, 2), EINVAL);
    expect_errno("context-rings-mask", context_init(second, mask_set, 2), EINVAL);
    expect_errno("context-unknown-param", context_init(second, unknown, 1), EINVAL);
    expect_errno("context-named-twice", context_init(second, named_twice, 2), EINVAL);

    const struct drm_virtgpu_context_set_param second_params[] = {
        {VIRTGPU_CONTEXT_PARAM_CAPSET_ID, CAPSET_VENUS},
        {VIRTGPU_CONTEXT_PARAM_NUM_RINGS, 64},
        {VIRTGPU_CONTEXT_PARAM_DEBUG_NAME, (uintptr_t)second_name},
    };
    printf(TAG ": context-name-second %s\n", second_name);
    if (expect_ok("context-second", context_init(second, second_params, 3)) == 0)
        pass("context-second", "its own Venus context, named by DEBUG_NAME");
    expect_errno("context-second-again", context_init(second, second_params, 3), EEXIST);
    expect_errno("context-first-after-second", context_init(first, venus_params, 3), EEXIST);

    check_display_refused(first, "first");
    check_display_refused(second, "second");
    check_gem_close(first, "first");
    check_gem_close(second, "second");

    struct drm_virtgpu_execbuffer exec = {0};
    expect_errno("submit-not-yet", ioctl(second, DRM_IOCTL_VIRTGPU_EXECBUFFER, &exec), EINVAL);

    printf(TAG ": closing second\n");
    close(second);
    check_version(first, "first-after-close");
    expect_errno("context-first-kept", context_init(first, venus_params, 3), EEXIST);
    printf(TAG ": closing first\n");
    close(first);

    static const char third_name[] = TAG "-3";
    const struct drm_virtgpu_context_set_param third_params[] = {
        {VIRTGPU_CONTEXT_PARAM_CAPSET_ID, CAPSET_VENUS},
        {VIRTGPU_CONTEXT_PARAM_DEBUG_NAME, (uintptr_t)third_name},
    };
    int third = open(path, O_RDWR | O_CLOEXEC);
    if (expect_ok("open-third", third) >= 0) {
        check_version(third, "third");
        printf(TAG ": context-name-third %s\n", third_name);
        if (expect_ok("context-third", context_init(third, third_params, 2)) == 0)
            pass("context-third", "a new context after the first two closed");
        printf(TAG ": closing third\n");
        close(third);
    }

    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
