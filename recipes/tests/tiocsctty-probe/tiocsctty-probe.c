#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <pty.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <unistd.h>

enum {
    MAX_READ_ITERS = 64,
    MAX_LINE_BUF = 512,
    MAX_WAIT_ITERS = 16,
    POLL_TIMEOUT_MS = 5000
};

static size_t read_line_bounded(int fd, char *buf, size_t cap) {
    size_t len = 0;
    if (cap == 0) {
        return 0;
    }
    buf[0] = '\0';
    for (int i = 0; i < MAX_READ_ITERS && len + 1 < cap; i++) {
        struct pollfd pfd;
        pfd.fd = fd;
        pfd.events = POLLIN;
        pfd.revents = 0;
        int prc = poll(&pfd, 1, POLL_TIMEOUT_MS);
        if (prc < 0) {
            if (errno == EINTR) {
                continue;
            }
            break;
        }
        if (prc == 0) {
            break;
        }
        char chunk[MAX_LINE_BUF];
        size_t want = cap - 1 - len;
        if (want > sizeof(chunk)) {
            want = sizeof(chunk);
        }
        ssize_t n = read(fd, chunk, want);
        if (n < 0) {
            if (errno == EINTR) {
                continue;
            }
            break;
        }
        if (n == 0) {
            break;
        }
        for (ssize_t j = 0; j < n && len + 1 < cap; j++) {
            if (chunk[j] != '\0') {
                buf[len++] = chunk[j];
            }
        }
        buf[len] = '\0';
        if (strchr(buf, '\n') != NULL) {
            break;
        }
    }
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r')) {
        len--;
        buf[len] = '\0';
    }
    return len;
}

static int wait_child_bounded(pid_t pid, int *status_out) {
    for (int i = 0; i < MAX_WAIT_ITERS; i++) {
        pid_t w = waitpid(pid, status_out, 0);
        if (w == pid) {
            return 0;
        }
        if (w < 0 && errno == EINTR) {
            continue;
        }
        return -1;
    }
    return -1;
}

static int check_forkpty(void) {
    int master = -1;
    char pty_name[PATH_MAX];
    memset(pty_name, 0, sizeof(pty_name));

    errno = 0;
    pid_t pid = forkpty(&master, pty_name, NULL, NULL);
    if (pid < 0) {
        int err = errno;
        printf("FAIL forkpty forkpty_errno=%d (%s)\n", err, strerror(err));
        return 0;
    }

    if (pid == 0) {
        pid_t cpid = getpid();
        pid_t csid = getsid(0);
        pid_t cpg = tcgetpgrp(0);
        const char *ctty = ttyname(0);
        printf(
            "FORKPTY_CHILD pid=%d sid=%d tcgetpgrp=%d tty=%s\n",
            (int)cpid,
            (int)csid,
            (int)cpg,
            ctty != NULL ? ctty : "(null)"
        );
        fflush(stdout);
        _exit(0);
    }

    const char *mpts_raw = ptsname(master);
    char master_pts[PATH_MAX];
    if (mpts_raw != NULL) {
        snprintf(master_pts, sizeof(master_pts), "%s", mpts_raw);
    } else {
        snprintf(master_pts, sizeof(master_pts), "%s", pty_name);
    }

    char line[MAX_LINE_BUF];
    read_line_bounded(master, line, sizeof(line));
    close(master);

    int status = 0;
    if (wait_child_bounded(pid, &status) != 0) {
        printf("FAIL forkpty waitpid_failed line=%s\n", line);
        return 0;
    }

    int cpid = -1;
    int csid = -1;
    int cpg = -1;
    char ctty[PATH_MAX];
    memset(ctty, 0, sizeof(ctty));

    int parsed = sscanf(
        line,
        "FORKPTY_CHILD pid=%d sid=%d tcgetpgrp=%d tty=%4095s",
        &cpid,
        &csid,
        &cpg,
        ctty
    );
    int tty_eq = (parsed == 4 && master_pts[0] != '\0' && strcmp(ctty, master_pts) == 0);
    int exit_ok = (WIFEXITED(status) && WEXITSTATUS(status) == 0);

    if (exit_ok && parsed == 4 && cpid == (int)pid && csid == (int)pid && cpg == (int)pid && tty_eq) {
        printf(
            "PASS forkpty pid=%d sid=%d tcgetpgrp=%d tty=%s tty_eq=1\n",
            cpid,
            csid,
            cpg,
            ctty
        );
        return 1;
    }

    printf(
        "FAIL forkpty status=%d parsed=%d expected_pid=%d cpid=%d csid=%d cpg=%d ctty=%s master_pts=%s tty_eq=%d line=%s\n",
        status,
        parsed,
        (int)pid,
        cpid,
        csid,
        cpg,
        ctty,
        master_pts,
        tty_eq,
        line
    );
    return 0;
}

static int check_direct_tiocsctty(void) {
    int master = -1;
    int sub = -1;
    char pty_name[PATH_MAX];
    memset(pty_name, 0, sizeof(pty_name));

    if (openpty(&master, &sub, pty_name, NULL, NULL) != 0) {
        printf("FAIL direct-tiocsctty openpty_errno=%d\n", errno);
        return 0;
    }

    const char *mpts_raw = ptsname(master);
    char master_pts[PATH_MAX];
    if (mpts_raw != NULL) {
        snprintf(master_pts, sizeof(master_pts), "%s", mpts_raw);
    } else {
        snprintf(master_pts, sizeof(master_pts), "%s", pty_name);
    }

    int pfd[2] = {-1, -1};
    if (pipe(pfd) != 0) {
        printf("FAIL direct-tiocsctty pipe_errno=%d\n", errno);
        close(sub);
        close(master);
        return 0;
    }

    pid_t pid = fork();
    if (pid < 0) {
        printf("FAIL direct-tiocsctty fork_errno=%d\n", errno);
        close(pfd[0]);
        close(pfd[1]);
        close(sub);
        close(master);
        return 0;
    }

    if (pid == 0) {
        close(master);
        close(pfd[0]);
        pid_t sid_rc = setsid();
        errno = 0;
        int rc = ioctl(sub, TIOCSCTTY, 0);
        int err = (rc < 0) ? errno : 0;
        pid_t cpid = getpid();
        pid_t csid = getsid(0);
        pid_t cpg = tcgetpgrp(sub);
        const char *ctty = ttyname(sub);
        char msg[MAX_LINE_BUF];
        int n = snprintf(
            msg,
            sizeof(msg),
            "DIRECT_CHILD sid_rc=%d rc=%d errno=%d pid=%d sid=%d tcgetpgrp=%d tty=%s\n",
            (int)sid_rc,
            rc,
            err,
            (int)cpid,
            (int)csid,
            (int)cpg,
            ctty != NULL ? ctty : "(null)"
        );
        if (n > 0) {
            size_t to_write = (size_t)n < sizeof(msg) ? (size_t)n : sizeof(msg) - 1;
            ssize_t ignored = write(pfd[1], msg, to_write);
            (void)ignored;
        }
        close(pfd[1]);
        close(sub);
        _exit(0);
    }

    close(sub);
    close(pfd[1]);

    char line[MAX_LINE_BUF];
    read_line_bounded(pfd[0], line, sizeof(line));
    close(pfd[0]);
    close(master);

    int status = 0;
    if (wait_child_bounded(pid, &status) != 0) {
        printf("FAIL direct-tiocsctty waitpid_failed line=%s\n", line);
        return 0;
    }

    int sid_rc = -1;
    int rc = -1;
    int err = -1;
    int cpid = -1;
    int csid = -1;
    int cpg = -1;
    char ctty[PATH_MAX];
    memset(ctty, 0, sizeof(ctty));

    int parsed = sscanf(
        line,
        "DIRECT_CHILD sid_rc=%d rc=%d errno=%d pid=%d sid=%d tcgetpgrp=%d tty=%4095s",
        &sid_rc,
        &rc,
        &err,
        &cpid,
        &csid,
        &cpg,
        ctty
    );
    int tty_eq = (parsed == 7 && master_pts[0] != '\0' && strcmp(ctty, master_pts) == 0);
    int exit_ok = (WIFEXITED(status) && WEXITSTATUS(status) == 0);

    if (exit_ok && parsed == 7 && sid_rc == (int)pid && rc == 0 && err == 0
        && cpid == (int)pid && csid == (int)pid && cpg == (int)pid && tty_eq) {
        printf(
            "PASS direct-tiocsctty pid=%d sid=%d tcgetpgrp=%d tty=%s tty_eq=1\n",
            cpid,
            csid,
            cpg,
            ctty
        );
        return 1;
    }

    printf(
        "FAIL direct-tiocsctty status=%d parsed=%d expected_pid=%d sid_rc=%d rc=%d errno=%d cpid=%d csid=%d cpg=%d ctty=%s master_pts=%s tty_eq=%d line=%s\n",
        status,
        parsed,
        (int)pid,
        sid_rc,
        rc,
        err,
        cpid,
        csid,
        cpg,
        ctty,
        master_pts,
        tty_eq,
        line
    );
    return 0;
}

int main(void) {
    int passed = 0;
    passed += check_forkpty();
    passed += check_direct_tiocsctty();
    printf("SUMMARY pass=%d total=2\n", passed);
    return passed == 2 ? 0 : 1;
}
