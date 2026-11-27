#include <assert.h>
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

enum {
    WL_DISPLAY_ID = 1,
    WL_REGISTRY_ID = 2,
    WL_SCAN_SYNC_ID = 3,
    WL_BIND_OBJ_ID = 3,
    WL_BIND_SYNC_ID = 4,
    WL_DISPLAY_SYNC_OP = 0,
    WL_DISPLAY_GET_REGISTRY_OP = 1,
    WL_DISPLAY_ERROR_EV = 0,
    WL_REGISTRY_BIND_OP = 0,
    WL_REGISTRY_GLOBAL_EV = 0,
    WL_CALLBACK_DONE_EV = 0,
    MAX_WIRE_BYTES = 65536,
    MAX_MSG_BYTES = 4096,
    MAX_IFACE_LEN = 256,
    MAX_ERR_MSG_LEN = 512,
    MAX_EVENTS = 1024,
    MAX_GLOBALS = 256,
    SCAN_TIMEOUT_MS = 5000,
    BIND_TIMEOUT_MS = 3000,
    MAX_PATH_LEN = 108,
};

struct iface_rule {
    const char *interface_name;
    const char *category;
};

static const struct iface_rule CORE_IFACES[] = {
    {"wl_compositor", "core"},
    {"wl_shm", "core"},
    {"xdg_wm_base", "core"},
    {"zwp_idle_inhibit_manager_v1", "core"},
};

static const struct iface_rule RESTRICTED_IFACES[] = {
    {"zwlr_screencopy_manager_v1", "capture"},
    {"ext_output_image_capture_source_manager_v1", "capture"},
    {"ext_image_copy_capture_manager_v1", "capture"},
    {"zwp_virtual_keyboard_manager_v1", "virtual_input"},
    {"zwlr_virtual_pointer_manager_v1", "virtual_input"},
    {"zwlr_data_control_manager_v1", "data_control"},
    {"ext_data_control_manager_v1", "data_control"},
    {"zwlr_layer_shell_v1", "layer_shell"},
    {"ext_session_lock_manager_v1", "session_lock"},
    {"ext_idle_notifier_v1", "idle_notify"},
    {"wp_security_context_manager_v1", "other_privileged"},
    {"zwp_input_method_manager_v2", "other_privileged"},
    {"zwlr_foreign_toplevel_manager_v1", "other_privileged"},
    {"ext_foreign_toplevel_list_v1", "other_privileged"},
    {"ext_workspace_manager_v1", "other_privileged"},
    {"zwlr_output_manager_v1", "other_privileged"},
    {"zwlr_gamma_control_manager_v1", "other_privileged"},
    {"mutter_x11_interop", "other_privileged"},
};

static const char *lookup_category(const struct iface_rule *rules, size_t count, const char *iface)
{
    for (size_t i = 0; i < count; i++) {
        if (strcmp(rules[i].interface_name, iface) == 0) {
            return rules[i].category;
        }
    }
    return NULL;
}

static uint32_t read_u32(const uint8_t *buf)
{
    uint32_t value = 0;
    memcpy(&value, buf, sizeof(value));
    return value;
}

static void write_u32(uint8_t *buf, uint32_t value)
{
    memcpy(buf, &value, sizeof(value));
}

static uint64_t monotonic_ms(void)
{
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

static int send_all(int fd, const uint8_t *buf, size_t len)
{
    assert(len <= MAX_MSG_BYTES);
    size_t sent = 0;
    while (sent < len) {
        ssize_t n = send(fd, buf + sent, len - sent, 0);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            return -1;
        }
        if (n == 0) {
            return -1;
        }
        sent += (size_t)n;
    }
    return 0;
}

static int connect_wayland(void)
{
    const char *runtime_dir = getenv("XDG_RUNTIME_DIR");
    const char *display = getenv("WAYLAND_DISPLAY");
    if (!display || !display[0]) {
        display = "wayland-0";
    }
    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    int written = 0;
    if (display[0] == '/') {
        written = snprintf(addr.sun_path, sizeof(addr.sun_path), "%s", display);
    } else {
        if (!runtime_dir || !runtime_dir[0]) {
            fprintf(stderr, "wayland-deny-probe: XDG_RUNTIME_DIR is unset\n");
            return -1;
        }
        written = snprintf(addr.sun_path, sizeof(addr.sun_path), "%s/%s", runtime_dir, display);
    }
    if (written <= 0 || written >= (int)sizeof(addr.sun_path) || written >= MAX_PATH_LEN) {
        fprintf(stderr, "wayland-deny-probe: socket path too long\n");
        return -1;
    }
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) {
        fprintf(stderr, "wayland-deny-probe: socket: %s\n", strerror(errno));
        return -1;
    }
    if (connect(fd, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        fprintf(stderr, "wayland-deny-probe: connect %s: %s\n", addr.sun_path, strerror(errno));
        close(fd);
        return -1;
    }
    return fd;
}

static int send_get_registry(int fd, uint32_t registry_id)
{
    uint8_t msg[12];
    write_u32(msg + 0, WL_DISPLAY_ID);
    write_u32(msg + 4, (12u << 16) | WL_DISPLAY_GET_REGISTRY_OP);
    write_u32(msg + 8, registry_id);
    return send_all(fd, msg, sizeof(msg));
}

static int send_sync(int fd, uint32_t callback_id)
{
    uint8_t msg[12];
    write_u32(msg + 0, WL_DISPLAY_ID);
    write_u32(msg + 4, (12u << 16) | WL_DISPLAY_SYNC_OP);
    write_u32(msg + 8, callback_id);
    return send_all(fd, msg, sizeof(msg));
}

static int send_registry_bind(int fd, uint32_t registry_id, uint32_t name, const char *iface, uint32_t version, uint32_t new_id)
{
    size_t slen = strlen(iface);
    if (slen == 0 || slen >= MAX_IFACE_LEN) {
        fprintf(stderr, "wayland-deny-probe: invalid interface length\n");
        return -1;
    }
    uint32_t wire_len = (uint32_t)(slen + 1u);
    uint32_t padded = (wire_len + 3u) & ~3u;
    uint32_t total = 24u + padded;
    assert(total <= MAX_MSG_BYTES);
    uint8_t msg[MAX_MSG_BYTES];
    memset(msg, 0, total);
    write_u32(msg + 0, registry_id);
    write_u32(msg + 4, (total << 16) | WL_REGISTRY_BIND_OP);
    write_u32(msg + 8, name);
    write_u32(msg + 12, wire_len);
    memcpy(msg + 16, iface, wire_len);
    write_u32(msg + 16 + padded, version);
    write_u32(msg + 20 + padded, new_id);
    return send_all(fd, msg, total);
}

struct wire_reader {
    int fd;
    uint8_t buf[MAX_WIRE_BYTES];
    size_t len;
};

static int wire_fill(struct wire_reader *reader, int timeout_ms)
{
    if (reader->len >= sizeof(reader->buf)) {
        return -2;
    }
    struct pollfd pfd = {reader->fd, POLLIN, 0};
    int ready = poll(&pfd, 1, timeout_ms);
    if (ready < 0) {
        if (errno == EINTR) {
            return 1;
        }
        return -1;
    }
    if (ready == 0) {
        return 0;
    }
    ssize_t n = recv(reader->fd, reader->buf + reader->len, sizeof(reader->buf) - reader->len, 0);
    if (n < 0) {
        if (errno == EINTR) {
            return 1;
        }
        return -1;
    }
    if (n == 0) {
        return -3;
    }
    reader->len += (size_t)n;
    return 1;
}

static void wire_consume(struct wire_reader *reader, size_t count)
{
    assert(count <= reader->len);
    size_t rem = reader->len - count;
    if (rem > 0) {
        memmove(reader->buf, reader->buf + count, rem);
    }
    reader->len = rem;
}

static void sanitize_text(char *dst, size_t dst_size, const uint8_t *src, size_t src_len)
{
    assert(dst_size > 1);
    size_t out = 0;
    for (size_t i = 0; i < src_len && out + 1 < dst_size; i++) {
        uint8_t ch = src[i];
        if (ch == 0) {
            break;
        }
        if (ch >= 32 && ch < 127) {
            dst[out++] = (char)ch;
        } else {
            dst[out++] = ' ';
        }
    }
    dst[out] = '\0';
}

static int run_scan(void)
{
    int fd = connect_wayland();
    if (fd < 0) {
        return 2;
    }
    if (send_get_registry(fd, WL_REGISTRY_ID) != 0 || send_sync(fd, WL_SCAN_SYNC_ID) != 0) {
        fprintf(stderr, "wayland-deny-probe: failed to send scan requests\n");
        close(fd);
        return 2;
    }

    struct wire_reader reader;
    reader.fd = fd;
    reader.len = 0;

    uint64_t deadline = monotonic_ms() + SCAN_TIMEOUT_MS;
    unsigned events = 0;
    unsigned globals = 0;
    int done = 0;

    while (!done && events < MAX_EVENTS) {
        while (reader.len >= 8 && !done && events < MAX_EVENTS) {
            uint32_t obj = read_u32(reader.buf + 0);
            uint32_t word1 = read_u32(reader.buf + 4);
            uint32_t opcode = word1 & 0xffffu;
            uint32_t size = word1 >> 16;
            if (size < 8 || (size & 3u) != 0 || size > MAX_MSG_BYTES) {
                fprintf(stderr, "wayland-deny-probe: invalid message size %u\n", size);
                close(fd);
                return 2;
            }
            if (reader.len < size) {
                break;
            }
            const uint8_t *payload = reader.buf + 8;
            size_t plen = size - 8;
            events++;

            if (obj == WL_DISPLAY_ID && opcode == WL_DISPLAY_ERROR_EV && plen >= 12) {
                uint32_t err_obj = read_u32(payload + 0);
                uint32_t err_code = read_u32(payload + 4);
                uint32_t str_len = read_u32(payload + 8);
                char msg[MAX_ERR_MSG_LEN];
                size_t avail = plen - 12;
                sanitize_text(msg, sizeof(msg), payload + 12, str_len < avail ? str_len : avail);
                fprintf(stderr, "wayland-deny-probe: scan protocol error obj=%u code=%u msg=%s\n", err_obj, err_code, msg);
                close(fd);
                return 2;
            }
            if (obj == WL_REGISTRY_ID && opcode == WL_REGISTRY_GLOBAL_EV && plen >= 12) {
                if (globals >= MAX_GLOBALS) {
                    fprintf(stderr, "wayland-deny-probe: exceeded MAX_GLOBALS\n");
                    close(fd);
                    return 2;
                }
                uint32_t name = read_u32(payload + 0);
                uint32_t str_len = read_u32(payload + 4);
                uint32_t padded = (str_len + 3u) & ~3u;
                if (str_len > 0 && str_len <= MAX_IFACE_LEN && 12u + padded <= plen) {
                    char iface[MAX_IFACE_LEN];
                    sanitize_text(iface, sizeof(iface), payload + 8, str_len);
                    uint32_t version = read_u32(payload + 8 + padded);
                    globals++;
                    printf("GLOBAL %u %s %u\n", name, iface, version);
                    if (lookup_category(CORE_IFACES, sizeof(CORE_IFACES) / sizeof(CORE_IFACES[0]), iface) != NULL) {
                        printf("CORE %u %s %u\n", name, iface, version);
                    }
                    const char *cat = lookup_category(RESTRICTED_IFACES, sizeof(RESTRICTED_IFACES) / sizeof(RESTRICTED_IFACES[0]), iface);
                    if (cat != NULL) {
                        printf("RESTRICTED %s %u %s %u\n", cat, name, iface, version);
                    }
                }
            }
            if (obj == WL_SCAN_SYNC_ID && opcode == WL_CALLBACK_DONE_EV) {
                done = 1;
            }
            wire_consume(&reader, size);
        }
        if (done) {
            break;
        }
        uint64_t now = monotonic_ms();
        if (now >= deadline) {
            fprintf(stderr, "wayland-deny-probe: scan timed out\n");
            close(fd);
            return 2;
        }
        int r = wire_fill(&reader, (int)(deadline - now));
        if (r <= 0) {
            fprintf(stderr, "wayland-deny-probe: scan connection closed (%d)\n", r);
            close(fd);
            return 2;
        }
    }

    close(fd);
    if (!done) {
        fprintf(stderr, "wayland-deny-probe: scan reached event bound without sync\n");
        return 2;
    }
    printf("SCAN_DONE globals=%u\n", globals);
    return 0;
}

static int run_bind(uint32_t name, const char *iface, uint32_t version, int expect_deny)
{
    int fd = connect_wayland();
    if (fd < 0) {
        return 2;
    }
    if (send_get_registry(fd, WL_REGISTRY_ID) != 0 ||
        send_registry_bind(fd, WL_REGISTRY_ID, name, iface, version, WL_BIND_OBJ_ID) != 0 ||
        send_sync(fd, WL_BIND_SYNC_ID) != 0) {
        fprintf(stderr, "wayland-deny-probe: failed to send bind requests\n");
        close(fd);
        return 2;
    }

    struct wire_reader reader;
    reader.fd = fd;
    reader.len = 0;

    uint64_t deadline = monotonic_ms() + BIND_TIMEOUT_MS;
    unsigned events = 0;

    while (events < MAX_EVENTS) {
        while (reader.len >= 8 && events < MAX_EVENTS) {
            uint32_t obj = read_u32(reader.buf + 0);
            uint32_t word1 = read_u32(reader.buf + 4);
            uint32_t opcode = word1 & 0xffffu;
            uint32_t size = word1 >> 16;
            if (size < 8 || (size & 3u) != 0 || size > MAX_MSG_BYTES) {
                fprintf(stderr, "wayland-deny-probe: invalid message size %u\n", size);
                close(fd);
                return 2;
            }
            if (reader.len < size) {
                break;
            }
            const uint8_t *payload = reader.buf + 8;
            size_t plen = size - 8;
            events++;

            if (obj == WL_DISPLAY_ID && opcode == WL_DISPLAY_ERROR_EV && plen >= 12) {
                uint32_t err_obj = read_u32(payload + 0);
                uint32_t err_code = read_u32(payload + 4);
                uint32_t str_len = read_u32(payload + 8);
                char msg[MAX_ERR_MSG_LEN];
                size_t avail = plen - 12;
                sanitize_text(msg, sizeof(msg), payload + 12, str_len < avail ? str_len : avail);
                printf("BIND_DENIED %u %s %u obj=%u code=%u msg=%s\n", name, iface, version, err_obj, err_code, msg);
                close(fd);
                return expect_deny ? 0 : 1;
            }
            if (obj == WL_BIND_SYNC_ID && opcode == WL_CALLBACK_DONE_EV) {
                printf("BIND_OK %u %s %u\n", name, iface, version);
                close(fd);
                return expect_deny ? 1 : 0;
            }
            wire_consume(&reader, size);
        }
        uint64_t now = monotonic_ms();
        if (now >= deadline) {
            printf("BIND_TIMEOUT %u %s %u\n", name, iface, version);
            close(fd);
            return 2;
        }
        int r = wire_fill(&reader, (int)(deadline - now));
        if (r == 0) {
            printf("BIND_TIMEOUT %u %s %u\n", name, iface, version);
            close(fd);
            return 2;
        }
        if (r < 0) {
            printf("BIND_DISCONNECT %u %s %u rc=%d\n", name, iface, version, r);
            close(fd);
            return 2;
        }
    }

    close(fd);
    fprintf(stderr, "wayland-deny-probe: bind reached event bound without result\n");
    return 2;
}

static int parse_u32(const char *text, uint32_t *out)
{
    char *end = NULL;
    errno = 0;
    unsigned long value = strtoul(text, &end, 10);
    if (errno != 0 || end == text || *end != '\0' || value > UINT32_MAX) {
        return -1;
    }
    *out = (uint32_t)value;
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "--scan") == 0) {
        return run_scan();
    }
    if (argc == 6 && strcmp(argv[1], "--bind") == 0) {
        uint32_t name = 0;
        uint32_t version = 0;
        if (parse_u32(argv[2], &name) != 0 || parse_u32(argv[4], &version) != 0) {
            fprintf(stderr, "wayland-deny-probe: invalid numeric argument\n");
            return 2;
        }
        int expect_deny = 0;
        if (strcmp(argv[5], "--expect-deny") == 0) {
            expect_deny = 1;
        } else if (strcmp(argv[5], "--expect-allow") == 0) {
            expect_deny = 0;
        } else {
            fprintf(stderr, "wayland-deny-probe: expected --expect-deny or --expect-allow\n");
            return 2;
        }
        return run_bind(name, argv[3], version, expect_deny);
    }
    fprintf(stderr, "Usage: wayland-deny-probe --scan | --bind <name> <interface> <version> --expect-allow|--expect-deny\n");
    return 2;
}
