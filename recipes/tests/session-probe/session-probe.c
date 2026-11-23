#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

extern intptr_t redox_fpath_v1(size_t fd, void *buf, size_t len);

enum {
    MAX_CHECK_FD = 1024,
    MAX_PATH_BUF = 512,
    MAX_LEAK_BUF = 1024
};

static const char *env_or_unset(const char *name) {
    const char *val = getenv(name);
    return (val != NULL && val[0] != '\0') ? val : "unset";
}

static void format_fpath(int fd, char *buf, size_t cap) {
    intptr_t n = redox_fpath_v1((size_t)fd, buf, cap - 1);
    if (n >= 0 && (size_t)n < cap) {
        buf[n] = '\0';
    } else {
        snprintf(buf, cap, "err=%ld", (long)n);
    }
}

int main(void) {
    char fd1_path[MAX_PATH_BUF];
    char fd2_path[MAX_PATH_BUF];
    format_fpath(1, fd1_path, sizeof(fd1_path));
    format_fpath(2, fd2_path, sizeof(fd2_path));

    char leaked[MAX_LEAK_BUF];
    size_t leak_len = 0;
    leaked[0] = '\0';

    for (int fd = 3; fd < MAX_CHECK_FD; fd++) {
        int fl = fcntl(fd, F_GETFD);
        if (fl < 0) {
            continue;
        }
        char fpath[MAX_PATH_BUF];
        format_fpath(fd, fpath, sizeof(fpath));
        int added = snprintf(
            leaked + leak_len,
            sizeof(leaked) - leak_len,
            "%s%d(%s)",
            leak_len == 0 ? "" : ",",
            fd,
            fpath
        );
        if (added > 0 && (size_t)added < sizeof(leaked) - leak_len) {
            leak_len += (size_t)added;
        }
    }

    int input_fd = open("/scheme/input/consumer", O_RDONLY | O_NONBLOCK);
    const char *input_status = "denied";
    if (input_fd >= 0) {
        input_status = "open";
        close(input_fd);
    }

    const char *runtime_dir = getenv("XDG_RUNTIME_DIR");
    if (runtime_dir == NULL || runtime_dir[0] == '\0') {
        return 2;
    }

    char tmp_path[MAX_PATH_BUF];
    char out_path[MAX_PATH_BUF];
    int n1 = snprintf(tmp_path, sizeof(tmp_path), "%s/spawn-probe.txt.tmp", runtime_dir);
    int n2 = snprintf(out_path, sizeof(out_path), "%s/spawn-probe.txt", runtime_dir);
    if (n1 <= 0 || (size_t)n1 >= sizeof(tmp_path) || n2 <= 0 || (size_t)n2 >= sizeof(out_path)) {
        return 3;
    }

    FILE *fp = fopen(tmp_path, "w");
    if (fp == NULL) {
        return 4;
    }

    const char *home = getenv("HOME");
    int home_ok = (home != NULL && home[0] != '\0' && access(home, W_OK) == 0);

    fprintf(fp, "id=uid=%u euid=%u gid=%u\n", (unsigned)getuid(), (unsigned)geteuid(), (unsigned)getgid());
    fprintf(fp, "fd1_path=%s\n", fd1_path);
    fprintf(fp, "fd2_path=%s\n", fd2_path);
    fprintf(fp, "env_consumer=%s\n", env_or_unset("FERRION_INPUT_CONSUMER_FD"));
    fprintf(fp, "env_login=%s\n", env_or_unset("FERRION_LOGIN_SESSION"));
    fprintf(fp, "env_init_notify=%s\n", env_or_unset("INIT_NOTIFY"));
    fprintf(fp, "env_tty=%s\n", env_or_unset("TTY"));
    fprintf(fp, "gsettings_backend=%s\n", env_or_unset("GSETTINGS_BACKEND"));
    fprintf(fp, "home=%s\n", env_or_unset("HOME"));
    fprintf(fp, "home_writable=%s\n", home_ok ? "yes" : "no");
    fprintf(fp, "xdg_runtime_dir=%s\n", env_or_unset("XDG_RUNTIME_DIR"));
    fprintf(fp, "wayland_display=%s\n", env_or_unset("WAYLAND_DISPLAY"));
    fprintf(fp, "input_scheme=%s\n", input_status);
    fprintf(fp, "open_fds=%s\n", leak_len == 0 ? "none" : leaked);

    if (fclose(fp) != 0) {
        return 5;
    }
    if (rename(tmp_path, out_path) != 0) {
        return 6;
    }
    return 0;
}
