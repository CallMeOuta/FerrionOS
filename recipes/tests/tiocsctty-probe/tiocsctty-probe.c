#define _GNU_SOURCE

#include <errno.h>
#include <limits.h>
#include <poll.h>
#include <pty.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    MAX_READ_ITERS = 64,
    MAX_LINE_BUF = 512,
    MAX_WAIT_ITERS = 16,
    MAX_SLEEP_ITERS = 16,
    POLL_TIMEOUT_MS = 5000,
    SIGWINCH_WAIT_NS = 100000000L
};

static volatile sig_atomic_t g_winch_count = 0;

static void handle_sigwinch(int signo) {
    (void)signo;
    g_winch_count++;
}

static void sleep_brief_bounded(void) {
    struct timespec req;
    struct timespec rem;
    req.tv_sec = 0;
    req.tv_nsec = SIGWINCH_WAIT_NS;
    for (int i = 0; i < MAX_SLEEP_ITERS; i++) {
        rem.tv_sec = 0;
        rem.tv_nsec = 0;
        if (nanosleep(&req, &rem) == 0) {
            return;
        }
        if (errno != EINTR) {
            return;
        }
        req = rem;
        if (req.tv_sec == 0 && req.tv_nsec == 0) {
            return;
        }
    }
}

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

static int check_sigwinch(void) {
    struct winsize orig_ws;
    memset(&orig_ws, 0, sizeof(orig_ws));
    if (ioctl(STDIN_FILENO, TIOCGWINSZ, &orig_ws) != 0) {
        int err = errno;
        printf("FAIL sigwinch tiocgwinsz_errno=%d (%s)\n", err, strerror(err));
        return 0;
    }

    pid_t my_pgrp = getpgrp();
    pid_t orig_pgrp = tcgetpgrp(STDIN_FILENO);
    int changed_pgrp = 0;
    struct sigaction ign_ttou;
    struct sigaction old_ttou;
    memset(&ign_ttou, 0, sizeof(ign_ttou));
    memset(&old_ttou, 0, sizeof(old_ttou));
    ign_ttou.sa_handler = SIG_IGN;
    sigemptyset(&ign_ttou.sa_mask);
    if (orig_pgrp != my_pgrp && my_pgrp > 0) {
        if (sigaction(SIGTTOU, &ign_ttou, &old_ttou) == 0) {
            if (tcsetpgrp(STDIN_FILENO, my_pgrp) == 0) {
                changed_pgrp = 1;
            }
            sigaction(SIGTTOU, &old_ttou, NULL);
        }
    }
    pid_t active_pgrp = tcgetpgrp(STDIN_FILENO);

    g_winch_count = 0;
    struct sigaction sa;
    struct sigaction old_sa;
    memset(&sa, 0, sizeof(sa));
    memset(&old_sa, 0, sizeof(old_sa));
    sa.sa_handler = handle_sigwinch;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGWINCH, &sa, &old_sa) != 0) {
        int err = errno;
        printf("FAIL sigwinch sigaction_errno=%d (%s)\n", err, strerror(err));
        return 0;
    }

    struct winsize ws1 = orig_ws;
    ws1.ws_row = (orig_ws.ws_row == 30) ? 31 : 30;
    ws1.ws_col = (orig_ws.ws_col == 100) ? 101 : 100;
    ws1.ws_xpixel = (unsigned short)(ws1.ws_col * 8);
    ws1.ws_ypixel = (unsigned short)(ws1.ws_row * 16);

    int rc1 = ioctl(STDIN_FILENO, TIOCSWINSZ, &ws1);
    int err1 = (rc1 != 0) ? errno : 0;
    sleep_brief_bounded();
    int c1 = (int)g_winch_count;

    struct winsize readback1;
    memset(&readback1, 0, sizeof(readback1));
    int rb1_rc = ioctl(STDIN_FILENO, TIOCGWINSZ, &readback1);

    int rc_same = ioctl(STDIN_FILENO, TIOCSWINSZ, &ws1);
    int err_same = (rc_same != 0) ? errno : 0;
    sleep_brief_bounded();
    int c_same = (int)g_winch_count;

    struct winsize ws2 = ws1;
    ws2.ws_row = (unsigned short)(ws1.ws_row + 1);
    ws2.ws_col = (unsigned short)(ws1.ws_col + 1);
    ws2.ws_xpixel = (unsigned short)(ws2.ws_col * 8);
    ws2.ws_ypixel = (unsigned short)(ws2.ws_row * 16);

    int rc2 = ioctl(STDIN_FILENO, TIOCSWINSZ, &ws2);
    int err2 = (rc2 != 0) ? errno : 0;
    sleep_brief_bounded();
    int c2 = (int)g_winch_count;

    struct winsize readback2;
    memset(&readback2, 0, sizeof(readback2));
    int rb2_rc = ioctl(STDIN_FILENO, TIOCGWINSZ, &readback2);

    sigaction(SIGWINCH, &old_sa, NULL);
    (void)ioctl(STDIN_FILENO, TIOCSWINSZ, &orig_ws);
    if (changed_pgrp && orig_pgrp > 0) {
        if (sigaction(SIGTTOU, &ign_ttou, &old_ttou) == 0) {
            (void)tcsetpgrp(STDIN_FILENO, orig_pgrp);
            sigaction(SIGTTOU, &old_ttou, NULL);
        }
    }

    int dims_ok = (rb1_rc == 0 && readback1.ws_row == ws1.ws_row && readback1.ws_col == ws1.ws_col
        && rb2_rc == 0 && readback2.ws_row == ws2.ws_row && readback2.ws_col == ws2.ws_col);
    int ioctl_ok = (rc1 == 0 && rc_same == 0 && rc2 == 0);
    int counts_ok = (c1 == 1 && c_same == 1 && c2 == 2);

    if (ioctl_ok && dims_ok && counts_ok) {
        printf(
            "PASS sigwinch counts=%d,%d,%d pgrp=%d active_pgrp=%d orig_pgrp=%d\n",
            c1,
            c_same,
            c2,
            (int)my_pgrp,
            (int)active_pgrp,
            (int)orig_pgrp
        );
        return 1;
    }

    printf(
        "FAIL sigwinch counts=%d,%d,%d rc=%d,%d,%d errno=%d,%d,%d dims_ok=%d pgrp=%d active_pgrp=%d orig_pgrp=%d\n",
        c1,
        c_same,
        c2,
        rc1,
        rc_same,
        rc2,
        err1,
        err_same,
        err2,
        dims_ok,
        (int)my_pgrp,
        (int)active_pgrp,
        (int)orig_pgrp
    );
    return 0;
}

int main(int argc, char **argv) {
    if (argc == 2 && strcmp(argv[1], "--sigwinch") == 0) {
        int ok = check_sigwinch();
        printf("SUMMARY pass=%d total=1\n", ok);
        return ok == 1 ? 0 : 1;
    }
    int passed = 0;
    passed += check_forkpty();
    passed += check_direct_tiocsctty();
    printf("SUMMARY pass=%d total=2\n", passed);
    return passed == 2 ? 0 : 1;
}
