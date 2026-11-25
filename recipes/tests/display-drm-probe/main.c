#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#define TAG "display-drm-probe"
#define MAX_CONNECTORS 32U
#define MAX_MODES 64U
#define MAX_PROPS 32U
#define MAX_ENCODERS 16U
#define SENTINEL_TYPE 99U
#define SENTINEL_ID 77U
#define DRM_MODE_CONNECTOR_VIRTUAL 15U

#define DRM_IOC(dir, nr, size) \
    ((unsigned long)(((unsigned long)(dir) << 30) | ((unsigned long)(size) << 16) | (0x64UL << 8) | (nr)))
#define DRM_IOWR(nr, type) DRM_IOC(3, nr, sizeof(type))

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

#define DRM_IOCTL_MODE_GETRESOURCES DRM_IOWR(0xa0, struct drm_mode_card_res)
#define DRM_IOCTL_MODE_GETCONNECTOR DRM_IOWR(0xa7, struct drm_mode_get_connector)

static const char *connector_type_prefix(uint32_t type)
{
    switch (type) {
    case 0: return "Unknown";
    case 1: return "VGA";
    case 2: return "DVI-I";
    case 3: return "DVI-D";
    case 4: return "DVI-A";
    case 5: return "Composite";
    case 6: return "SVIDEO";
    case 7: return "LVDS";
    case 8: return "Component";
    case 9: return "9PinDIN";
    case 10: return "DP";
    case 11: return "HDMI-A";
    case 12: return "HDMI-B";
    case 13: return "TV";
    case 14: return "eDP";
    case 15: return "Virtual";
    case 16: return "DSI";
    case 17: return "DPI";
    case 18: return "Writeback";
    case 19: return "SPI";
    case 20: return "USB";
    default: return "Invalid";
    }
}

int main(int argc, char **argv)
{
    int check_virtual = 0;
    uint32_t min_virtual = 2U;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--check-virtual") == 0) {
            check_virtual = 1;
        } else if (strcmp(argv[i], "--min-virtual") == 0 && i + 1 < argc) {
            unsigned long val = strtoul(argv[++i], NULL, 10);
            if (val > MAX_CONNECTORS) {
                fprintf(stderr, TAG ": min-virtual exceeds %u\n", MAX_CONNECTORS);
                return 2;
            }
            min_virtual = (uint32_t)val;
        }
    }

    setvbuf(stdout, NULL, _IOLBF, 0);

    int input_fd = open("/scheme/input/consumer", O_RDONLY | O_NONBLOCK | O_CLOEXEC);
    if (input_fd < 0) {
        printf(TAG ": FAIL open-consumer: %s\n", strerror(errno));
        printf(TAG ": FAIL\n");
        return 1;
    }
    int drm_fd = openat(input_fd, "display", O_RDWR | O_NONBLOCK | O_CLOEXEC);
    if (drm_fd < 0) {
        printf(TAG ": FAIL open-display: %s\n", strerror(errno));
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    uint32_t fbs[MAX_CONNECTORS], crtcs[MAX_CONNECTORS], conns[MAX_CONNECTORS], encs[MAX_CONNECTORS];
    struct drm_mode_card_res res;
    memset(&res, 0, sizeof res);
    res.fb_id_ptr = (uintptr_t)fbs;
    res.crtc_id_ptr = (uintptr_t)crtcs;
    res.connector_id_ptr = (uintptr_t)conns;
    res.encoder_id_ptr = (uintptr_t)encs;
    res.count_fbs = MAX_CONNECTORS;
    res.count_crtcs = MAX_CONNECTORS;
    res.count_connectors = MAX_CONNECTORS;
    res.count_encoders = MAX_CONNECTORS;

    if (ioctl(drm_fd, DRM_IOCTL_MODE_GETRESOURCES, &res) < 0) {
        printf(TAG ": FAIL get-resources: %s\n", strerror(errno));
        close(drm_fd);
        close(input_fd);
        printf(TAG ": FAIL\n");
        return 1;
    }

    uint32_t nconns = res.count_connectors;
    if (nconns > MAX_CONNECTORS)
        nconns = MAX_CONNECTORS;
    printf(TAG ": resources connectors=%u crtcs=%u encoders=%u\n",
           res.count_connectors, res.count_crtcs, res.count_encoders);

    int failures = 0;
    uint32_t virtual_count = 0;

    for (uint32_t i = 0; i < nconns; i++) {
        uint32_t c_encs[MAX_ENCODERS];
        struct drm_mode_modeinfo modes[MAX_MODES];
        uint32_t props[MAX_PROPS];
        uint64_t prop_vals[MAX_PROPS];

        struct drm_mode_get_connector probe;
        memset(&probe, 0, sizeof probe);
        probe.connector_id = conns[i];
        probe.connector_type = SENTINEL_TYPE;
        probe.connector_type_id = SENTINEL_ID;
        if (ioctl(drm_fd, DRM_IOCTL_MODE_GETCONNECTOR, &probe) < 0) {
            printf(TAG ": FAIL probe-conn-%u: %s\n", conns[i], strerror(errno));
            failures++;
            continue;
        }

        struct drm_mode_get_connector zero_req;
        memset(&zero_req, 0, sizeof zero_req);
        zero_req.connector_id = conns[i];
        zero_req.count_modes = 1;
        zero_req.modes_ptr = (uintptr_t)modes;
        if (ioctl(drm_fd, DRM_IOCTL_MODE_GETCONNECTOR, &zero_req) < 0) {
            printf(TAG ": FAIL zero-conn-%u: %s\n", conns[i], strerror(errno));
            failures++;
            continue;
        }

        struct drm_mode_get_connector full;
        memset(&full, 0, sizeof full);
        full.encoders_ptr = (uintptr_t)c_encs;
        full.modes_ptr = (uintptr_t)modes;
        full.props_ptr = (uintptr_t)props;
        full.prop_values_ptr = (uintptr_t)prop_vals;
        full.count_modes = MAX_MODES;
        full.count_props = MAX_PROPS;
        full.count_encoders = MAX_ENCODERS;
        full.connector_id = conns[i];
        full.connector_type = SENTINEL_TYPE;
        full.connector_type_id = SENTINEL_ID;
        if (ioctl(drm_fd, DRM_IOCTL_MODE_GETCONNECTOR, &full) < 0) {
            printf(TAG ": FAIL full-conn-%u: %s\n", conns[i], strerror(errno));
            failures++;
            continue;
        }

        printf("CONN id=%u probe_sentinel=(%u,%u) zero_req=(%u,%u) type=%u type_id=%u name=%s-%u status=%u mm=%ux%u modes=%u\n",
               conns[i],
               probe.connector_type, probe.connector_type_id,
               zero_req.connector_type, zero_req.connector_type_id,
               full.connector_type, full.connector_type_id,
               connector_type_prefix(full.connector_type), full.connector_type_id,
               full.connection, full.mm_width, full.mm_height, full.count_modes);

        uint32_t nmodes = full.count_modes;
        if (nmodes > MAX_MODES)
            nmodes = MAX_MODES;
        for (uint32_t m = 0; m < nmodes; m++) {
            char mname[33];
            memcpy(mname, modes[m].name, 32);
            mname[32] = '\0';
            printf("MODE conn=%u idx=%u %ux%u clock=%u htotal=%u vtotal=%u vrefresh=%u type=0x%x flags=0x%x name=%s\n",
                   conns[i], m,
                   (unsigned)modes[m].hdisplay, (unsigned)modes[m].vdisplay,
                   modes[m].clock, (unsigned)modes[m].htotal, (unsigned)modes[m].vtotal,
                   modes[m].vrefresh, modes[m].type, modes[m].flags, mname);
        }

        if (check_virtual) {
            if (probe.connector_type == SENTINEL_TYPE || probe.connector_type_id == SENTINEL_ID ||
                full.connector_type == SENTINEL_TYPE || full.connector_type_id == SENTINEL_ID) {
                printf(TAG ": FAIL conn-%u-sentinel: echoed (%u,%u)\n",
                       conns[i], full.connector_type, full.connector_type_id);
                failures++;
            } else if (probe.connector_type != full.connector_type ||
                       probe.connector_type_id != full.connector_type_id ||
                       zero_req.connector_type != full.connector_type ||
                       zero_req.connector_type_id != full.connector_type_id) {
                printf(TAG ": FAIL conn-%u-consistent: probe=(%u,%u) zero=(%u,%u) full=(%u,%u)\n",
                       conns[i],
                       probe.connector_type, probe.connector_type_id,
                       zero_req.connector_type, zero_req.connector_type_id,
                       full.connector_type, full.connector_type_id);
                failures++;
            } else if (full.connector_type != DRM_MODE_CONNECTOR_VIRTUAL) {
                printf(TAG ": FAIL conn-%u-type: got %u expected %u\n",
                       conns[i], full.connector_type, DRM_MODE_CONNECTOR_VIRTUAL);
                failures++;
            } else {
                virtual_count++;
                if (full.connector_type_id != virtual_count) {
                    printf(TAG ": FAIL conn-%u-type-id: got %u expected %u\n",
                           conns[i], full.connector_type_id, virtual_count);
                    failures++;
                } else {
                    printf(TAG ": pass conn-%u: Virtual-%u\n", conns[i], full.connector_type_id);
                }
            }
        }
    }

    if (check_virtual && virtual_count < min_virtual) {
        printf(TAG ": FAIL virtual-count: got %u expected >= %u\n", virtual_count, min_virtual);
        failures++;
    }

    close(drm_fd);
    close(input_fd);
    if (failures > 0) {
        printf(TAG ": FAIL\n");
        return 1;
    }
    printf(TAG ": PASS\n");
    return 0;
}
