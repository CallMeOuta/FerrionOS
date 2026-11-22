#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#define TAG "gpu-probe-flip"
#define BLOCKING_READ_LIMIT_S 5

enum blocking_step {
    BLOCKING_UNSUPPORTED,
    BLOCKING_SET_FAILED,
    BLOCKING_ARMED,
    BLOCKING_DONE,
};

static volatile sig_atomic_t blocking_timed_out;

static void on_blocking_alarm(int sig)
{
    (void)sig;
    blocking_timed_out = 1;
}

#define DRM_IOC(dir, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | (0x64UL << 8) | (nr)))
#define DRM_IOWR(nr, type) DRM_IOC(3, nr, sizeof(type))

struct drm_get_cap {
    uint64_t capability;
    uint64_t value;
};

struct drm_wait_vblank_request {
    uint32_t type;
    uint32_t sequence;
    unsigned long signal;
};

struct drm_wait_vblank_reply {
    uint32_t type;
    uint32_t sequence;
    long tval_sec;
    long tval_usec;
};

union drm_wait_vblank {
    struct drm_wait_vblank_request request;
    struct drm_wait_vblank_reply reply;
};

struct drm_event {
    uint32_t type;
    uint32_t length;
};

struct drm_event_vblank {
    struct drm_event base;
    uint64_t user_data;
    uint32_t tv_sec;
    uint32_t tv_usec;
    uint32_t sequence;
    uint32_t crtc_id;
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

struct drm_mode_get_encoder {
    uint32_t encoder_id;
    uint32_t encoder_type;
    uint32_t crtc_id;
    uint32_t possible_crtcs;
    uint32_t possible_clones;
};

struct drm_mode_get_connector {
    uint64_t encoders_ptr;
    uint64_t modes_ptr;
    uint64_t props_ptr;
    uint64_t prop_values_ptr;
    uint32_t count_modes;
    uint32_t count_props;
    uint32_t count_encoders;
    uint32_t encoder_id;
    uint32_t connector_id;
    uint32_t connector_type;
    uint32_t connector_type_id;
    uint32_t connection;
    uint32_t mm_width;
    uint32_t mm_height;
    uint32_t subpixel;
    uint32_t pad;
};

struct drm_mode_fb_cmd {
    uint32_t fb_id;
    uint32_t width;
    uint32_t height;
    uint32_t pitch;
    uint32_t bpp;
    uint32_t depth;
    uint32_t handle;
};

struct drm_mode_crtc_page_flip {
    uint32_t crtc_id;
    uint32_t fb_id;
    uint32_t flags;
    uint32_t reserved;
    uint64_t user_data;
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

struct drm_mode_map_dumb {
    uint32_t handle;
    uint32_t pad;
    uint64_t offset;
};

struct drm_mode_destroy_dumb {
    uint32_t handle;
};

struct inputd_control_event {
    size_t kind;
    size_t data;
};

#define DRM_CAP_TIMESTAMP_MONOTONIC 0x06
#define DRM_CAP_CRTC_IN_VBLANK_EVENT 0x12

#define DRM_IOCTL_GET_CAP DRM_IOWR(0x0c, struct drm_get_cap)
#define DRM_IOCTL_WAIT_VBLANK DRM_IOWR(0x3a, union drm_wait_vblank)
#define DRM_IOCTL_MODE_GETRESOURCES DRM_IOWR(0xa0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_SETCRTC DRM_IOWR(0xa2, struct drm_mode_crtc)
#define DRM_IOCTL_MODE_GETENCODER DRM_IOWR(0xa6, struct drm_mode_get_encoder)
#define DRM_IOCTL_MODE_GETCONNECTOR DRM_IOWR(0xa7, struct drm_mode_get_connector)
#define DRM_IOCTL_MODE_ADDFB DRM_IOWR(0xae, struct drm_mode_fb_cmd)
#define DRM_IOCTL_MODE_RMFB DRM_IOWR(0xaf, unsigned int)
#define DRM_IOCTL_MODE_PAGE_FLIP DRM_IOWR(0xb0, struct drm_mode_crtc_page_flip)
#define DRM_IOCTL_MODE_CREATE_DUMB DRM_IOWR(0xb2, struct drm_mode_create_dumb)
#define DRM_IOCTL_MODE_MAP_DUMB DRM_IOWR(0xb3, struct drm_mode_map_dumb)
#define DRM_IOCTL_MODE_DESTROY_DUMB DRM_IOWR(0xb4, struct drm_mode_destroy_dumb)

#define DRM_MODE_CONNECTED 1
#define DRM_MODE_PAGE_FLIP_EVENT 0x01
#define DRM_EVENT_VBLANK 0x01
#define DRM_EVENT_FLIP_COMPLETE 0x02

#define DRM_VBLANK_RELATIVE 0x1
#define DRM_VBLANK_EVENT 0x4000000

#define FLIP_COUNT 600
#define COLOR_EVEN 0x00FF6020U
#define COLOR_ODD 0x0020A0FFU

extern ssize_t redox_fpath(int fd, void *buf, size_t count);

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

static int cmp_u64(const void *a, const void *b)
{
    uint64_t lhs = *(const uint64_t *)a;
    uint64_t rhs = *(const uint64_t *)b;
    return (lhs > rhs) - (lhs < rhs);
}

static int activate_vt_from_drm_fd(int drm_fd, size_t *vt_out)
{
    char path[256];
    ssize_t n = redox_fpath(drm_fd, path, sizeof path - 1);
    if (n <= 0)
        return -1;
    path[n] = '\0';
    const char *slash = strrchr(path, '/');
    if (!slash || slash[1] == '\0')
        return -1;
    char *end = NULL;
    unsigned long vt = strtoul(slash + 1, &end, 10);
    if (!end || *end != '\0')
        return -1;
    int ctrl = open("/scheme/input/control", O_WRONLY | O_CLOEXEC);
    if (ctrl < 0)
        return -1;
    struct inputd_control_event cmd = {.kind = 1, .data = (size_t)vt};
    ssize_t written = write(ctrl, &cmd, sizeof cmd);
    int saved = errno;
    close(ctrl);
    errno = saved;
    if (written != (ssize_t)sizeof cmd)
        return -1;
    *vt_out = (size_t)vt;
    printf(TAG ": display %s vt %zu\n", path, *vt_out);
    return 0;
}

static int create_color_fb(int drm_fd, uint32_t width, uint32_t height, uint32_t color,
                           uint32_t *handle_out, uint32_t *fb_out, void **map_out, uint64_t *size_out)
{
    struct drm_mode_create_dumb dumb = {.height = height, .width = width, .bpp = 32, .flags = 0};
    if (ioctl(drm_fd, DRM_IOCTL_MODE_CREATE_DUMB, &dumb) < 0)
        return -1;

    struct drm_mode_map_dumb map_req = {.handle = dumb.handle, .pad = 0, .offset = 0};
    if (ioctl(drm_fd, DRM_IOCTL_MODE_MAP_DUMB, &map_req) < 0) {
        struct drm_mode_destroy_dumb destroy = {.handle = dumb.handle};
        ioctl(drm_fd, DRM_IOCTL_MODE_DESTROY_DUMB, &destroy);
        return -1;
    }

    void *ptr = mmap(NULL, (size_t)dumb.size, PROT_READ | PROT_WRITE, MAP_SHARED, drm_fd, (off_t)map_req.offset);
    if (ptr == MAP_FAILED) {
        struct drm_mode_destroy_dumb destroy = {.handle = dumb.handle};
        ioctl(drm_fd, DRM_IOCTL_MODE_DESTROY_DUMB, &destroy);
        return -1;
    }

    uint32_t *pixels = (uint32_t *)ptr;
    size_t count = (size_t)dumb.size / sizeof(uint32_t);
    for (size_t i = 0; i < count; i++)
        pixels[i] = color;

    struct drm_mode_fb_cmd fb = {
        .width = width,
        .height = height,
        .pitch = dumb.pitch,
        .bpp = 32,
        .depth = 24,
        .handle = dumb.handle,
    };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_ADDFB, &fb) < 0) {
        munmap(ptr, (size_t)dumb.size);
        struct drm_mode_destroy_dumb destroy = {.handle = dumb.handle};
        ioctl(drm_fd, DRM_IOCTL_MODE_DESTROY_DUMB, &destroy);
        return -1;
    }

    *handle_out = dumb.handle;
    *fb_out = fb.fb_id;
    *map_out = ptr;
    *size_out = dumb.size;
    return 0;
}

static int wait_for_event(int drm_fd, struct drm_event_vblank *ev, int timeout_ms)
{
    struct pollfd pfd = {.fd = drm_fd, .events = POLLIN, .revents = 0};
    int pr = poll(&pfd, 1, timeout_ms);
    if (pr <= 0)
        return pr == 0 ? -ETIMEDOUT : -errno;
    if (!(pfd.revents & POLLIN))
        return -EIO;
    ssize_t n = read(drm_fd, ev, sizeof *ev);
    if (n < 0)
        return -errno;
    if (n != (ssize_t)sizeof *ev)
        return -EIO;
    return 0;
}

static void wait_stdin_line(void)
{
    char line[64];
    (void)fgets(line, sizeof line, stdin);
}

int gpu_probe_flip(int argc, char **argv)
{
    int hold = 0;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--hold") == 0)
            hold = 1;
    }
    setvbuf(stdout, NULL, _IOLBF, 0);

    int input_fd = open("/scheme/input/consumer", O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (input_fd < 0) {
        fail("open-consumer", strerror(errno));
        printf(TAG ": FAIL\n");
        return 1;
    }
    int drm_fd = openat(input_fd, "display", O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if (drm_fd < 0) {
        fail("open-display", strerror(errno));
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    size_t vt = 0;
    if (activate_vt_from_drm_fd(drm_fd, &vt) < 0) {
        fail("activate-vt", strerror(errno));
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }
    pass("open-vt", "acquired consumer and activated VT");

    uint32_t fbs[32], crtcs[32], conns[32], encs[32];
    struct drm_mode_card_res res = {
        .fb_id_ptr = (uintptr_t)fbs,
        .crtc_id_ptr = (uintptr_t)crtcs,
        .connector_id_ptr = (uintptr_t)conns,
        .encoder_id_ptr = (uintptr_t)encs,
        .count_fbs = 32,
        .count_crtcs = 32,
        .count_connectors = 32,
        .count_encoders = 32,
    };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_GETRESOURCES, &res) < 0 || res.count_connectors == 0 || res.count_crtcs == 0) {
        fail("get-resources", strerror(errno));
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    uint32_t conn_id = 0, crtc_id = 0;
    struct drm_mode_modeinfo chosen_mode;
    memset(&chosen_mode, 0, sizeof chosen_mode);
    int found_conn = 0;
    for (uint32_t i = 0; i < res.count_connectors && i < 32; i++) {
        uint32_t c_encs[16];
        struct drm_mode_modeinfo modes[32];
        uint32_t props[32];
        uint64_t prop_vals[32];
        struct drm_mode_get_connector conn = {
            .encoders_ptr = (uintptr_t)c_encs,
            .modes_ptr = (uintptr_t)modes,
            .props_ptr = (uintptr_t)props,
            .prop_values_ptr = (uintptr_t)prop_vals,
            .count_modes = 32,
            .count_props = 32,
            .count_encoders = 16,
            .connector_id = conns[i],
        };
        if (ioctl(drm_fd, DRM_IOCTL_MODE_GETCONNECTOR, &conn) < 0)
            continue;
        if (conn.connection != DRM_MODE_CONNECTED || conn.count_modes == 0 || conn.count_encoders == 0)
            continue;
        struct drm_mode_get_encoder enc = {.encoder_id = c_encs[0]};
        if (ioctl(drm_fd, DRM_IOCTL_MODE_GETENCODER, &enc) < 0)
            continue;
        for (uint32_t c = 0; c < res.count_crtcs && c < 32; c++) {
            if (enc.possible_crtcs & (1U << c)) {
                conn_id = conns[i];
                crtc_id = crtcs[c];
                chosen_mode = modes[0];
                found_conn = 1;
                break;
            }
        }
        if (found_conn)
            break;
    }
    if (!found_conn) {
        fail("find-output", "no connected output with mode and CRTC");
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    uint32_t width = chosen_mode.hdisplay;
    uint32_t height = chosen_mode.vdisplay;
    char detail[256];
    snprintf(detail, sizeof detail, "crtc=%u conn=%u mode=%ux%u", crtc_id, conn_id, width, height);
    pass("output", detail);

    uint32_t handle0 = 0, handle1 = 0, fb0 = 0, fb1 = 0;
    void *map0 = NULL, *map1 = NULL;
    uint64_t size0 = 0, size1 = 0;
    if (create_color_fb(drm_fd, width, height, COLOR_EVEN, &handle0, &fb0, &map0, &size0) < 0 ||
        create_color_fb(drm_fd, width, height, COLOR_ODD, &handle1, &fb1, &map1, &size1) < 0) {
        fail("create-fbs", strerror(errno));
        goto cleanup;
    }
    pass("create-fbs", "two dumb framebuffers allocated");

    struct drm_mode_crtc set_crtc = {
        .set_connectors_ptr = (uintptr_t)&conn_id,
        .count_connectors = 1,
        .crtc_id = crtc_id,
        .fb_id = fb0,
        .mode_valid = 1,
        .mode = chosen_mode,
    };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_SETCRTC, &set_crtc) < 0) {
        fail("set-crtc", strerror(errno));
        goto cleanup;
    }
    pass("set-crtc", "initial framebuffer scanned out");

    struct drm_mode_crtc_page_flip first_flip = {
        .crtc_id = crtc_id,
        .fb_id = fb1,
        .flags = DRM_MODE_PAGE_FLIP_EVENT,
        .reserved = 0,
        .user_data = 0x464C495000000001ULL,
    };
    if (ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &first_flip) < 0) {
        snprintf(detail, sizeof detail, "failed with %s", strerror(errno));
        fail("flip-1", detail);
        goto cleanup;
    }
    struct drm_event_vblank ev;
    int werr = wait_for_event(drm_fd, &ev, 5000);
    if (werr != 0) {
        snprintf(detail, sizeof detail, "event 1 failed: %s", strerror(-werr));
        fail("event-1", detail);
        goto cleanup;
    }
    if (ev.base.type != DRM_EVENT_FLIP_COMPLETE || ev.base.length != sizeof ev ||
        ev.user_data != 0x464C495000000001ULL || ev.crtc_id != crtc_id) {
        snprintf(detail, sizeof detail, "bad event 1: type=%u len=%u user=%#llx crtc=%u",
                 ev.base.type, ev.base.length, (unsigned long long)ev.user_data, ev.crtc_id);
        fail("event-1", detail);
        goto cleanup;
    }

    uint64_t intervals[FLIP_COUNT - 1];
    uint64_t prev_ts = (uint64_t)ev.tv_sec * 1000000ULL + (uint64_t)ev.tv_usec;
    uint32_t prev_seq = ev.sequence;
    uint32_t seq_start = ev.sequence;
    int events_ok = 1;

    for (int i = 2; i <= FLIP_COUNT; i++) {
        uint64_t cookie = 0x464C495000000000ULL | (uint64_t)i;
        struct drm_mode_crtc_page_flip flip = {
            .crtc_id = crtc_id,
            .fb_id = (i & 1) ? fb1 : fb0,
            .flags = DRM_MODE_PAGE_FLIP_EVENT,
            .reserved = 0,
            .user_data = cookie,
        };
        if (ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &flip) < 0) {
            snprintf(detail, sizeof detail, "flip %d ioctl failed: %s", i, strerror(errno));
            fail("flip-loop", detail);
            goto cleanup;
        }
        werr = wait_for_event(drm_fd, &ev, 5000);
        if (werr != 0) {
            snprintf(detail, sizeof detail, "flip %d event failed: %s", i, strerror(-werr));
            fail("flip-loop", detail);
            goto cleanup;
        }
        uint64_t ts = (uint64_t)ev.tv_sec * 1000000ULL + (uint64_t)ev.tv_usec;
        if (ev.base.type != DRM_EVENT_FLIP_COMPLETE || ev.base.length != sizeof ev ||
            ev.user_data != cookie || ev.crtc_id != crtc_id ||
            ev.sequence <= prev_seq || ts < prev_ts) {
            snprintf(detail, sizeof detail,
                     "flip %d bad event: type=%u len=%u user=%#llx crtc=%u seq=%u(prev %u) ts=%llu(prev %llu)",
                     i, ev.base.type, ev.base.length, (unsigned long long)ev.user_data,
                     ev.crtc_id, ev.sequence, prev_seq, (unsigned long long)ts, (unsigned long long)prev_ts);
            fail("flip-loop", detail);
            goto cleanup;
        }
        intervals[i - 2] = ts - prev_ts;
        prev_ts = ts;
        prev_seq = ev.sequence;
        events_ok++;
    }

    qsort(intervals, FLIP_COUNT - 1, sizeof intervals[0], cmp_u64);
    uint64_t min_us = intervals[0];
    uint64_t median_us = intervals[(FLIP_COUNT - 1) / 2];
    uint64_t max_us = intervals[FLIP_COUNT - 2];
    printf(TAG ": flips %d events %d median_us %llu min_us %llu max_us %llu seq_start %u seq_end %u\n",
           FLIP_COUNT, events_ok, (unsigned long long)median_us, (unsigned long long)min_us,
           (unsigned long long)max_us, seq_start, prev_seq);
    snprintf(detail, sizeof detail, "%d events, median %llu us", events_ok, (unsigned long long)median_us);
    pass("flip-600", detail);

    struct pollfd empty_pfd = {.fd = drm_fd, .events = POLLIN, .revents = 0};
    int empty_pr = poll(&empty_pfd, 1, 0);
    if (empty_pr == 0)
        pass("poll-empty", "poll returned 0 with no queued events");
    else {
        snprintf(detail, sizeof detail, "poll returned %d revents=%#x", empty_pr, empty_pfd.revents);
        fail("poll-empty", detail);
    }

    ssize_t empty_rd = read(drm_fd, &ev, sizeof ev);
    if (empty_rd < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
        pass("read-empty-nonblock", "EAGAIN with no queued events");
    else {
        snprintf(detail, sizeof detail, "read returned %zd errno=%s", empty_rd, strerror(errno));
        fail("read-empty-nonblock", detail);
    }

    struct drm_get_cap cap = {.capability = DRM_CAP_CRTC_IN_VBLANK_EVENT, .value = 0};
    if (ioctl(drm_fd, DRM_IOCTL_GET_CAP, &cap) == 0 && cap.value == 1)
        pass("cap-crtc-in-vblank", "1");
    else {
        snprintf(detail, sizeof detail, "value=%llu errno=%s", (unsigned long long)cap.value, strerror(errno));
        fail("cap-crtc-in-vblank", detail);
    }

    struct drm_mode_crtc_page_flip bad_crtc = {.crtc_id = 0x7FFFFFFFU, .fb_id = fb0, .flags = DRM_MODE_PAGE_FLIP_EVENT};
    expect_errno("flip-bad-crtc", ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &bad_crtc), ENOENT);

    struct drm_mode_crtc_page_flip bad_fb = {.crtc_id = crtc_id, .fb_id = 0x7FFFFFFFU, .flags = DRM_MODE_PAGE_FLIP_EVENT};
    expect_errno("flip-bad-fb", ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &bad_fb), ENOENT);

    struct drm_mode_crtc_page_flip bad_flags = {.crtc_id = crtc_id, .fb_id = fb0, .flags = 0x100U};
    expect_errno("flip-bad-flags", ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &bad_flags), EINVAL);

    struct drm_mode_crtc_page_flip bad_reserved = {.crtc_id = crtc_id, .fb_id = fb0, .flags = 0, .reserved = 1};
    expect_errno("flip-bad-reserved", ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &bad_reserved), EINVAL);

    union drm_wait_vblank vbl;
    memset(&vbl, 0, sizeof vbl);
    vbl.request.type = DRM_VBLANK_RELATIVE;
    vbl.request.sequence = 0;
    if (ioctl(drm_fd, DRM_IOCTL_WAIT_VBLANK, &vbl) == 0 && vbl.reply.sequence >= prev_seq) {
        snprintf(detail, sizeof detail, "seq=%u sec=%ld usec=%ld",
                 vbl.reply.sequence, vbl.reply.tval_sec, vbl.reply.tval_usec);
        pass("wait-vblank-query", detail);
    } else {
        snprintf(detail, sizeof detail, "failed or regressed: seq=%u errno=%s", vbl.reply.sequence, strerror(errno));
        fail("wait-vblank-query", detail);
    }

    memset(&vbl, 0, sizeof vbl);
    vbl.request.type = DRM_VBLANK_RELATIVE | DRM_VBLANK_EVENT;
    vbl.request.sequence = 0;
    vbl.request.signal = 0x56424C4B00000001UL;
    if (ioctl(drm_fd, DRM_IOCTL_WAIT_VBLANK, &vbl) == 0 && wait_for_event(drm_fd, &ev, 5000) == 0 &&
        ev.base.type == DRM_EVENT_VBLANK && ev.base.length == sizeof ev &&
        ev.user_data == 0x56424C4B00000001ULL && ev.crtc_id == crtc_id) {
        snprintf(detail, sizeof detail, "seq=%u crtc=%u", ev.sequence, ev.crtc_id);
        pass("wait-vblank-event", detail);
    } else {
        fail("wait-vblank-event", "vblank event missing or malformed");
    }

    if (hold && failures == 0) {
        printf(TAG ": READY even\n");
        fflush(stdout);
        wait_stdin_line();

        struct drm_mode_crtc_page_flip odd_flip = {
            .crtc_id = crtc_id,
            .fb_id = fb1,
            .flags = DRM_MODE_PAGE_FLIP_EVENT,
            .reserved = 0,
            .user_data = 0x464C495000000601ULL,
        };
        if (ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &odd_flip) < 0 ||
            wait_for_event(drm_fd, &ev, 5000) != 0 ||
            ev.base.type != DRM_EVENT_FLIP_COMPLETE || ev.user_data != 0x464C495000000601ULL) {
            fail("flip-odd", "odd flip or event failed");
            goto cleanup;
        }
        prev_seq = ev.sequence;
        pass("flip-odd", "flipped to odd framebuffer");
        printf(TAG ": READY odd\n");
        fflush(stdout);
        wait_stdin_line();
    }

    enum blocking_step step = BLOCKING_SET_FAILED;
    int fl_err = 0;
    int fl = fcntl(drm_fd, F_GETFL);
    if (fl < 0) {
        fl_err = errno;
        step = (fl_err == EOPNOTSUPP) ? BLOCKING_UNSUPPORTED : BLOCKING_SET_FAILED;
    } else if (fcntl(drm_fd, F_SETFL, fl & ~O_NONBLOCK) < 0) {
        fl_err = errno;
        step = (fl_err == EOPNOTSUPP) ? BLOCKING_UNSUPPORTED : BLOCKING_SET_FAILED;
    } else {
        step = BLOCKING_ARMED;
    }

    switch (step) {
    case BLOCKING_UNSUPPORTED:
        printf(TAG ": unsupported read-blocking: %s\n", strerror(fl_err));
        step = BLOCKING_DONE;
        break;
    case BLOCKING_SET_FAILED:
        snprintf(detail, sizeof detail, "could not clear O_NONBLOCK: %s", strerror(fl_err));
        fail("read-blocking", detail);
        step = BLOCKING_DONE;
        break;
    case BLOCKING_ARMED: {
        struct drm_mode_crtc_page_flip blk_flip = {
            .crtc_id = crtc_id,
            .fb_id = fb0,
            .flags = DRM_MODE_PAGE_FLIP_EVENT,
            .reserved = 0,
            .user_data = 0x464C495000000602ULL,
        };
        if (ioctl(drm_fd, DRM_IOCTL_MODE_PAGE_FLIP, &blk_flip) == 0) {
            struct sigaction sa, old_sa;
            memset(&sa, 0, sizeof sa);
            sa.sa_handler = on_blocking_alarm;
            blocking_timed_out = 0;
            sigaction(SIGALRM, &sa, &old_sa);
            alarm(BLOCKING_READ_LIMIT_S);
            ssize_t n = read(drm_fd, &ev, sizeof ev);
            int rd_err = errno;
            alarm(0);
            sigaction(SIGALRM, &old_sa, NULL);
            if (n < 0 && rd_err == EINTR && blocking_timed_out)
                fail("read-blocking", "timed out");
            else if (n == 0)
                fail("read-blocking", "blocking read returned no event");
            else if (n == (ssize_t)sizeof ev && ev.base.type == DRM_EVENT_FLIP_COMPLETE &&
                     ev.user_data == 0x464C495000000602ULL && ev.sequence > prev_seq)
                pass("read-blocking", "blocking read delivered flip event");
            else {
                snprintf(detail, sizeof detail, "blocking read failed or returned bad event (n=%zd errno=%s)",
                         n, n < 0 ? strerror(rd_err) : "none");
                fail("read-blocking", detail);
            }
        } else {
            fail("read-blocking", strerror(errno));
        }
        (void)fcntl(drm_fd, F_SETFL, fl);
        step = BLOCKING_DONE;
        break;
    }
    case BLOCKING_DONE:
        break;
    }

cleanup:
    if (fb0) {
        unsigned int id = fb0;
        (void)ioctl(drm_fd, DRM_IOCTL_MODE_RMFB, &id);
    }
    if (fb1) {
        unsigned int id = fb1;
        (void)ioctl(drm_fd, DRM_IOCTL_MODE_RMFB, &id);
    }
    if (map0)
        munmap(map0, (size_t)size0);
    if (map1)
        munmap(map1, (size_t)size1);
    if (handle0) {
        struct drm_mode_destroy_dumb d = {.handle = handle0};
        (void)ioctl(drm_fd, DRM_IOCTL_MODE_DESTROY_DUMB, &d);
    }
    if (handle1) {
        struct drm_mode_destroy_dumb d = {.handle = handle1};
        (void)ioctl(drm_fd, DRM_IOCTL_MODE_DESTROY_DUMB, &d);
    }
    close(drm_fd);
    close(input_fd);

    printf(TAG ": %s\n", failures ? "FAIL" : "PASS");
    return failures ? 1 : 0;
}
