#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef F_OFD_GETLK
#define F_OFD_GETLK 36
#define F_OFD_SETLK 37
#define F_OFD_SETLKW 38
#endif

static char path[128];
static int failures;
static int up[2] = {-1, -1};
static int down[2] = {-1, -1};

#define READY_LIMIT_MS 30000

struct child {
    pid_t pid;
    int ready_fd;
};

enum ready {
    READY,
    CHILD_EXITED,
    TIMED_OUT,
};

static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
    }
}

static double now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec + ts.tv_nsec / 1e9;
}

static void result(const char *name, const char *problem) {
    if (problem) {
        failures++;
        printf("lock-probe: FAIL %s: %s\n", name, problem);
    } else {
        printf("lock-probe: pass %s\n", name);
    }
    fflush(stdout);
}

static int open_file(void) {
    int fd = open(path, O_RDWR | O_CREAT, 0644);
    if (fd < 0) {
        printf("lock-probe: FAIL open %s: %s\n", path, strerror(errno));
        exit(2);
    }
    return fd;
}

static void fresh(const char *name) {
    snprintf(path, sizeof path, "/tmp/lock-probe-%s-%d", name, (int)getpid());
    unlink(path);
    close(open_file());
}

static int range_lock(int fd, int cmd, short type, short whence, off_t start, off_t len) {
    struct flock fl;
    memset(&fl, 0, sizeof fl);
    fl.l_type = type;
    fl.l_whence = whence;
    fl.l_start = start;
    fl.l_len = len;
    return fcntl(fd, cmd, &fl) == 0 ? 0 : errno;
}

static int setlk(int fd, short type, off_t start, off_t len) {
    return range_lock(fd, F_SETLK, type, SEEK_SET, start, len);
}

static int busy(int err) {
    if (err == EAGAIN || err == EACCES) {
        return 1;
    }
    printf("lock-probe: expected a conflict, got %s\n", err ? strerror(err) : "success");
    return 0;
}

static int flock_err(int fd, int op) {
    return flock(fd, op) == 0 ? 0 : errno;
}

static struct flock getlk(int fd, int cmd, short type, off_t start, off_t len) {
    struct flock fl;
    memset(&fl, 0, sizeof fl);
    fl.l_type = type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    if (fcntl(fd, cmd, &fl) != 0) {
        fl.l_type = -1;
        fl.l_pid = errno;
    }
    return fl;
}

static int post_parent(int fd) {
    char byte = 1;
    return write(fd, &byte, 1) == 1 ? 0 : -1;
}

static void post(int fd) {
    char byte = 1;
    if (write(fd, &byte, 1) != 1) {
        _exit(90);
    }
}

static void await_byte(int fd) {
    char byte;
    if (read(fd, &byte, 1) != 1) {
        _exit(91);
    }
}

static void close_fd(int *fd) {
    if (*fd >= 0) {
        close(*fd);
        *fd = -1;
    }
}

static void close_pipes(void) {
    close_fd(&up[0]);
    close_fd(&up[1]);
    close_fd(&down[0]);
    close_fd(&down[1]);
}

static int open_pipes(void) {
    up[0] = up[1] = down[0] = down[1] = -1;
    if (pipe(up) != 0) {
        return -1;
    }
    if (pipe(down) != 0) {
        close_pipes();
        return -1;
    }
    return 0;
}

static int spawn_child(int (*body)(int), int arg, int with_pipes, struct child *out) {
    out->pid = -1;
    out->ready_fd = -1;
    if (with_pipes && open_pipes() != 0) {
        return -1;
    }
    fflush(stdout);
    pid_t pid = fork();
    if (pid < 0) {
        if (with_pipes) {
            close_pipes();
        }
        return -1;
    }
    if (pid == 0) {
        if (with_pipes) {
            close_fd(&up[0]);
            close_fd(&down[1]);
        }
        alarm(20);
        _exit(body(arg));
    }
    if (with_pipes) {
        close_fd(&up[1]);
        close_fd(&down[0]);
        out->ready_fd = up[0];
    }
    out->pid = pid;
    return 0;
}

static enum ready await_ready(struct child *c, int limit_ms) {
    struct pollfd pfd = {.fd = c->ready_fd, .events = POLLIN, .revents = 0};
    int r = poll(&pfd, 1, limit_ms);
    if (r == 0) {
        return TIMED_OUT;
    }
    if (r < 0) {
        return CHILD_EXITED;
    }
    char byte = 0;
    ssize_t n = read(c->ready_fd, &byte, 1);
    return n == 1 ? READY : CHILD_EXITED;
}

static const char *ready_problem(enum ready r) {
    switch (r) {
    case READY:
        return NULL;
    case CHILD_EXITED:
        return "child exited before signaling ready";
    case TIMED_OUT:
        return "child timed out before signaling ready";
    }
    return "child ready check failed";
}

static int reap(pid_t pid, int seconds) {
    assert(pid > 0);
    double deadline = now() + seconds;
    int status;
    while (now() < deadline) {
        pid_t done = waitpid(pid, &status, WNOHANG);
        if (done == pid) {
            return WIFEXITED(status) ? WEXITSTATUS(status) : 100 + WTERMSIG(status);
        }
        sleep_ms(20);
    }
    kill(pid, SIGKILL);
    waitpid(pid, &status, 0);
    return 99;
}

static char detail[256];

static const char *describe(const char *fmt, ...) {
    va_list args;
    va_start(args, fmt);
    vsnprintf(detail, sizeof detail, fmt, args);
    va_end(args);
    return detail;
}

static const char *child_status(int status) {
    if (status == 0) {
        return NULL;
    }
    if (status == 98) {
        return "fork failed";
    }
    if (status == 99) {
        return "child did not finish (blocked or hung)";
    }
    return describe("child check %d failed", status);
}

static int run(int (*body)(int), int arg, int seconds) {
    struct child c;
    if (spawn_child(body, arg, 0, &c) != 0) {
        return 98;
    }
    return reap(c.pid, seconds);
}

static int child_conflict(int unused) {
    (void)unused;
    int fd = open_file();
    if (!busy(setlk(fd, F_WRLCK, 5, 10))) {
        return 1;
    }
    if (!busy(setlk(fd, F_RDLCK, 9, 1))) {
        return 2;
    }
    if (setlk(fd, F_WRLCK, 10, 10) != 0) {
        return 3;
    }
    return 0;
}

static void case_conflict(void) {
    fresh("conflict");
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 10) != 0) {
        result("fcntl-conflict", describe("F_SETLK F_WRLCK failed: %s", strerror(errno)));
        close(fd);
        return;
    }
    result("fcntl-conflict", child_status(run(child_conflict, 0, 10)));
    close(fd);
}

static int child_getlk(int parent) {
    int fd = open_file();
    struct flock fl = getlk(fd, F_GETLK, F_WRLCK, 0, 0);
    if (fl.l_type != F_WRLCK || fl.l_pid != parent || fl.l_start != 0 || fl.l_len != 10 ||
        fl.l_whence != SEEK_SET) {
        printf("lock-probe: getlk saw type %d pid %d start %lld len %lld\n", fl.l_type, fl.l_pid,
               (long long)fl.l_start, (long long)fl.l_len);
        return 1;
    }
    fl = getlk(fd, F_GETLK, F_RDLCK, 20, 5);
    if (fl.l_type != F_UNLCK) {
        return 2;
    }
    fl = getlk(fd, F_GETLK, F_WRLCK, 25, 1);
    if (fl.l_type != F_RDLCK || fl.l_start != 20 || fl.l_len != 10 || fl.l_pid != parent) {
        return 3;
    }
    fl = getlk(fd, F_GETLK, F_WRLCK, 40, 0);
    if (fl.l_type != F_UNLCK) {
        return 4;
    }
    return 0;
}

static void case_getlk(void) {
    fresh("getlk");
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 10) != 0 || setlk(fd, F_RDLCK, 20, 10) != 0) {
        result("getlk-holder", "F_SETLK failed");
        close(fd);
        return;
    }
    struct flock own = getlk(fd, F_GETLK, F_WRLCK, 0, 0);
    if (own.l_type != F_UNLCK) {
        result("getlk-holder", "a process's own lock was reported as a conflict");
        close(fd);
        return;
    }
    result("getlk-holder", child_status(run(child_getlk, (int)getpid(), 10)));
    close(fd);
}

static int child_shared(int unused) {
    (void)unused;
    int fd = open_file();
    if (setlk(fd, F_RDLCK, 0, 100) != 0) {
        return 1;
    }
    if (!busy(setlk(fd, F_WRLCK, 50, 1))) {
        return 2;
    }
    return 0;
}

static void case_shared(void) {
    fresh("shared");
    int fd = open_file();
    if (setlk(fd, F_RDLCK, 0, 100) != 0) {
        result("fcntl-shared", "F_SETLK F_RDLCK failed");
    } else {
        result("fcntl-shared", child_status(run(child_shared, 0, 10)));
    }
    close(fd);
}

static int child_split(int stage) {
    int fd = open_file();
    if (stage == 0) {
        if (setlk(fd, F_WRLCK, 40, 20) != 0) {
            return 1;
        }
        if (!busy(setlk(fd, F_WRLCK, 39, 1))) {
            return 2;
        }
        if (!busy(setlk(fd, F_WRLCK, 60, 1))) {
            return 3;
        }
    } else if (stage == 1) {
        if (setlk(fd, F_RDLCK, 0, 20) != 0) {
            return 4;
        }
        if (!busy(setlk(fd, F_WRLCK, 10, 1))) {
            return 5;
        }
        if (!busy(setlk(fd, F_RDLCK, 20, 1))) {
            return 6;
        }
        if (!busy(setlk(fd, F_RDLCK, 150, 1))) {
            return 7;
        }
    } else if (setlk(fd, F_WRLCK, 0, 0) != 0) {
        return 8;
    }
    setlk(fd, F_UNLCK, 0, 0);
    return 0;
}

static void case_split(void) {
    fresh("split");
    int fd = open_file();
    const char *problem = NULL;
    if (setlk(fd, F_WRLCK, 0, 100) != 0 || setlk(fd, F_UNLCK, 40, 20) != 0) {
        problem = "parent lock or unlock failed";
    }
    if (!problem) {
        problem = child_status(run(child_split, 0, 10));
    }
    if (!problem && (setlk(fd, F_RDLCK, 0, 20) != 0 || setlk(fd, F_WRLCK, 100, 100) != 0)) {
        problem = "parent downgrade or extension failed";
    }
    if (!problem) {
        problem = child_status(run(child_split, 1, 10));
    }
    if (!problem && setlk(fd, F_UNLCK, 0, 0) != 0) {
        problem = "parent unlock of the whole file failed";
    }
    if (!problem) {
        problem = child_status(run(child_split, 2, 10));
    }
    result("fcntl-split-merge", problem);
    close(fd);
}

static int child_lock_whole(int expect_busy) {
    int fd = open_file();
    int err = setlk(fd, F_WRLCK, 0, 0);
    if (expect_busy) {
        return busy(err) ? 0 : 1;
    }
    if (err != 0) {
        printf("lock-probe: expected the lock, got %s\n", strerror(err));
    }
    return err == 0 ? 0 : 1;
}

static void case_close(void) {
    fresh("close");
    int fd = open_file();
    const char *problem = NULL;
    if (setlk(fd, F_WRLCK, 0, 0) != 0) {
        problem = "F_SETLK failed";
    } else if (run(child_lock_whole, 1, 10) != 0) {
        problem = "another process could lock while it was held";
    } else {
        close(fd);
        fd = -1;
        if (run(child_lock_whole, 0, 10) != 0) {
            problem = "the lock outlived close() of its descriptor";
        }
    }
    result("release-on-close", problem);
    if (fd >= 0) {
        close(fd);
    }

    fresh("close-other");
    fd = open_file();
    problem = NULL;
    if (setlk(fd, F_WRLCK, 0, 0) != 0) {
        problem = "F_SETLK failed";
    } else {
        close(open_file());
        if (run(child_lock_whole, 0, 10) != 0) {
            problem = "closing another descriptor of the file did not release the process's lock";
        }
    }
    result("release-on-close-other-fd", problem);
    close(fd);
}

static void case_path_lookups(void) {
    fresh("lookups");
    int fd = open_file();
    struct stat st;
    const char *problem = NULL;
    if (setlk(fd, F_WRLCK, 0, 0) != 0) {
        problem = "F_SETLK failed";
    } else if (stat(path, &st) != 0 || lstat(path, &st) != 0 || access(path, F_OK) != 0 ||
               access(path, R_OK | W_OK) != 0) {
        problem = describe("looking the file up failed: %s", strerror(errno));
    } else {
        int handle = open(path, O_PATH);
        if (handle < 0) {
            problem = describe("open O_PATH failed: %s", strerror(errno));
        } else {
            close(handle);
            if (run(child_lock_whole, 1, 10) != 0) {
                problem = "stat(), access() or closing an O_PATH descriptor released the process's lock";
            }
        }
    }
    result("path-lookups-keep-locks", problem);
    close(fd);
}

static int child_hold_and_exit(int how) {
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 0) != 0) {
        return 1;
    }
    struct stat st;
    fstat(fd, &st);
    fsync(fd);
    if (how == 1) {
        post(up[1]);
        pause();
    }
    _exit(0);
}

static void case_exit(void) {
    fresh("exit");
    const char *problem = child_status(run(child_hold_and_exit, 0, 10));
    if (!problem && run(child_lock_whole, 0, 10) != 0) {
        problem = "a lock outlived the process that held it";
    }
    result("release-on-exit", problem);

    fresh("kill");
    struct child c;
    if (spawn_child(child_hold_and_exit, 1, 1, &c) != 0) {
        result("release-on-kill", "fork failed");
        return;
    }
    enum ready r = await_ready(&c, READY_LIMIT_MS);
    if (r != READY) {
        reap(c.pid, 10);
        close_pipes();
        result("release-on-kill", ready_problem(r));
        return;
    }
    kill(c.pid, SIGKILL);
    reap(c.pid, 10);
    close_pipes();
    result("release-on-kill",
           run(child_lock_whole, 0, 10) == 0 ? NULL : "a lock outlived the killed process that held it");
}

static int child_setlkw(int arg) {
    int cmd = arg & 0xff;
    close(arg >> 8);
    int fd = open_file();
    post(up[1]);
    double start = now();
    if (range_lock(fd, cmd, F_WRLCK, SEEK_SET, 0, 1) != 0) {
        printf("lock-probe: waiting lock returned %s\n", strerror(errno));
        return 1;
    }
    if (now() - start < 0.3) {
        return 2;
    }
    return 0;
}

static void case_setlkw(void) {
    fresh("setlkw");
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 1) != 0) {
        result("setlkw-wakes", "F_SETLK failed");
        close(fd);
        return;
    }
    struct child c;
    if (spawn_child(child_setlkw, F_SETLKW | fd << 8, 1, &c) != 0) {
        result("setlkw-wakes", "fork failed");
    } else {
        enum ready r = await_ready(&c, READY_LIMIT_MS);
        if (r != READY) {
            setlk(fd, F_UNLCK, 0, 1);
            reap(c.pid, 10);
            close_pipes();
            result("setlkw-wakes", ready_problem(r));
        } else {
            sleep_ms(1000);
            setlk(fd, F_UNLCK, 0, 1);
            int status = reap(c.pid, 10);
            close_pipes();
            result("setlkw-wakes", status == 2 ? "F_SETLKW returned before the lock was released"
                                               : child_status(status));
        }
    }
    close(fd);

    fresh("ofd-setlkw");
    fd = open_file();
    if (range_lock(fd, F_OFD_SETLK, F_WRLCK, SEEK_SET, 0, 1) != 0) {
        result("ofd-setlkw-wakes", "F_OFD_SETLK failed");
        close(fd);
        return;
    }
    if (spawn_child(child_setlkw, F_OFD_SETLKW | fd << 8, 1, &c) != 0) {
        close(fd);
        result("ofd-setlkw-wakes", "fork failed");
        return;
    }
    enum ready r = await_ready(&c, READY_LIMIT_MS);
    if (r != READY) {
        close(fd);
        reap(c.pid, 10);
        close_pipes();
        result("ofd-setlkw-wakes", ready_problem(r));
        return;
    }
    sleep_ms(1000);
    close(fd);
    int status = reap(c.pid, 10);
    close_pipes();
    result("ofd-setlkw-wakes", status == 2 ? "F_OFD_SETLKW returned before the lock was released"
                                           : child_status(status));
}

static int child_setlkw_downgrade(int arg) {
    int cmd = arg & 0xff;
    close(arg >> 8);
    int fd = open_file();
    post(up[1]);
    double start = now();
    if (range_lock(fd, cmd, F_RDLCK, SEEK_SET, 0, 1) != 0) {
        printf("lock-probe: waiting read lock returned %s\n", strerror(errno));
        return 1;
    }
    if (now() - start < 0.3) {
        return 2;
    }
    return 0;
}

static void case_setlkw_downgrade(void) {
    fresh("setlkw-downgrade");
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 1) != 0) {
        result("setlkw-wakes-on-downgrade", "F_SETLK F_WRLCK failed");
        close(fd);
    } else {
        struct child c;
        if (spawn_child(child_setlkw_downgrade, F_SETLKW | fd << 8, 1, &c) != 0) {
            result("setlkw-wakes-on-downgrade", "fork failed");
        } else {
            enum ready r = await_ready(&c, READY_LIMIT_MS);
            if (r != READY) {
                setlk(fd, F_UNLCK, 0, 1);
                reap(c.pid, 5);
                close_pipes();
                result("setlkw-wakes-on-downgrade", ready_problem(r));
            } else {
                sleep_ms(1000);
                int err = setlk(fd, F_RDLCK, 0, 1);
                int status = reap(c.pid, 5);
                close_pipes();
                if (err != 0) {
                    result("setlkw-wakes-on-downgrade",
                           describe("parent downgrade failed: %s", strerror(err)));
                } else {
                    result("setlkw-wakes-on-downgrade",
                           status == 2 ? "F_SETLKW returned before the lock was downgraded"
                                       : child_status(status));
                }
            }
        }
        close(fd);
    }

    fresh("ofd-setlkw-downgrade");
    fd = open_file();
    if (range_lock(fd, F_OFD_SETLK, F_WRLCK, SEEK_SET, 0, 1) != 0) {
        result("ofd-setlkw-wakes-on-downgrade", "F_OFD_SETLK F_WRLCK failed");
        close(fd);
        return;
    }
    struct child c;
    if (spawn_child(child_setlkw_downgrade, F_OFD_SETLKW | fd << 8, 1, &c) != 0) {
        close(fd);
        result("ofd-setlkw-wakes-on-downgrade", "fork failed");
        return;
    }
    enum ready r = await_ready(&c, READY_LIMIT_MS);
    if (r != READY) {
        close(fd);
        reap(c.pid, 5);
        close_pipes();
        result("ofd-setlkw-wakes-on-downgrade", ready_problem(r));
        return;
    }
    sleep_ms(1000);
    int err = range_lock(fd, F_OFD_SETLK, F_RDLCK, SEEK_SET, 0, 1);
    int status = reap(c.pid, 5);
    close_pipes();
    close(fd);
    if (err != 0) {
        result("ofd-setlkw-wakes-on-downgrade",
               describe("parent OFD downgrade failed: %s", strerror(err)));
    } else {
        result("ofd-setlkw-wakes-on-downgrade",
               status == 2 ? "F_OFD_SETLKW returned before the lock was downgraded"
                           : child_status(status));
    }
}

static enum ready await_fd_byte(int fd, int limit_ms) {
    struct pollfd pfd = {.fd = fd, .events = POLLIN, .revents = 0};
    int r = poll(&pfd, 1, limit_ms);
    if (r == 0) {
        return TIMED_OUT;
    }
    if (r < 0) {
        return CHILD_EXITED;
    }
    char byte = 0;
    return read(fd, &byte, 1) == 1 ? READY : CHILD_EXITED;
}

static void case_setlkw_cascade(void) {
    fresh("setlkw-cascade");
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 10) != 0) {
        result("setlkw-wakes-after-cascaded-downgrade", "parent F_SETLK [0,10) failed");
        close(fd);
        return;
    }
    int c2_hold[2] = {-1, -1};
    int c2_go[2] = {-1, -1};
    int c2_park[2] = {-1, -1};
    int c2_woke[2] = {-1, -1};
    int c2_done[2] = {-1, -1};
    int c1_park[2] = {-1, -1};
    if (pipe(c2_hold) != 0 || pipe(c2_go) != 0 || pipe(c2_park) != 0 || pipe(c2_woke) != 0 ||
        pipe(c2_done) != 0 || pipe(c1_park) != 0) {
        close_fd(&c2_hold[0]);
        close_fd(&c2_hold[1]);
        close_fd(&c2_go[0]);
        close_fd(&c2_go[1]);
        close_fd(&c2_park[0]);
        close_fd(&c2_park[1]);
        close_fd(&c2_woke[0]);
        close_fd(&c2_woke[1]);
        close_fd(&c2_done[0]);
        close_fd(&c2_done[1]);
        close_fd(&c1_park[0]);
        close_fd(&c1_park[1]);
        close(fd);
        result("setlkw-wakes-after-cascaded-downgrade", "pipe failed");
        return;
    }

    fflush(stdout);
    pid_t c2 = fork();
    if (c2 == 0) {
        close(fd);
        close_fd(&c2_hold[0]);
        close_fd(&c2_go[1]);
        close_fd(&c2_park[0]);
        close_fd(&c2_woke[0]);
        close_fd(&c2_done[1]);
        close_fd(&c1_park[0]);
        close_fd(&c1_park[1]);
        alarm(30);
        int cfd = open_file();
        if (setlk(cfd, F_WRLCK, 10, 10) != 0) {
            _exit(11);
        }
        post(c2_hold[1]);
        await_byte(c2_go[0]);
        post(c2_park[1]);
        double start = now();
        if (range_lock(cfd, F_SETLKW, F_RDLCK, SEEK_SET, 0, 20) != 0) {
            _exit(12);
        }
        if (now() - start < 0.2) {
            _exit(13);
        }
        post(c2_woke[1]);
        await_byte(c2_done[0]);
        close(cfd);
        _exit(0);
    }

    const char *problem = NULL;
    pid_t c1 = -1;
    close_fd(&c2_hold[1]);
    close_fd(&c2_go[0]);
    close_fd(&c2_park[1]);
    close_fd(&c2_woke[1]);
    close_fd(&c2_done[0]);

    if (c2 < 0) {
        problem = "fork c2 failed";
    } else {
        enum ready r = await_fd_byte(c2_hold[0], READY_LIMIT_MS);
        if (r != READY) {
            problem = ready_problem(r);
        }
    }

    if (!problem) {
        fflush(stdout);
        c1 = fork();
        if (c1 == 0) {
            close(fd);
            close_fd(&c2_hold[0]);
            close_fd(&c2_go[1]);
            close_fd(&c2_park[0]);
            close_fd(&c2_woke[0]);
            close_fd(&c2_done[1]);
            close_fd(&c1_park[0]);
            alarm(20);
            int cfd = open_file();
            post(c1_park[1]);
            double start = now();
            if (range_lock(cfd, F_SETLKW, F_RDLCK, SEEK_SET, 10, 10) != 0) {
                _exit(21);
            }
            if (now() - start < 0.2) {
                _exit(22);
            }
            close(cfd);
            _exit(0);
        }
        close_fd(&c1_park[1]);
        if (c1 < 0) {
            problem = "fork c1 failed";
        } else {
            enum ready r = await_fd_byte(c1_park[0], READY_LIMIT_MS);
            if (r != READY) {
                problem = ready_problem(r);
            }
        }
    } else {
        close_fd(&c1_park[1]);
    }

    if (!problem) {
        sleep_ms(500);
        if (post_parent(c2_go[1]) != 0) {
            problem = "failed to signal c2";
        } else {
            enum ready r = await_fd_byte(c2_park[0], READY_LIMIT_MS);
            if (r != READY) {
                problem = ready_problem(r);
            }
        }
    }

    if (!problem) {
        sleep_ms(500);
        int err = setlk(fd, F_UNLCK, 0, 10);
        if (err != 0) {
            problem = describe("parent unlock [0,10) failed: %s", strerror(err));
        } else {
            enum ready r = await_fd_byte(c2_woke[0], 5000);
            if (r != READY) {
                problem = "c2 did not wake after parent unlock";
            } else {
                int c1_status = reap(c1, 5);
                c1 = -1;
                if (c1_status == 22) {
                    problem = "c1 returned before c2 downgraded";
                } else {
                    problem = child_status(c1_status);
                }
            }
        }
    }

    if (c2 > 0) {
        post_parent(c2_done[1]);
        int c2_status = reap(c2, 5);
        if (!problem && c2_status != 0) {
            problem = child_status(c2_status);
        }
    }
    if (c1 > 0) {
        kill(c1, SIGKILL);
        reap(c1, 5);
    }
    close_fd(&c2_hold[0]);
    close_fd(&c2_go[1]);
    close_fd(&c2_park[0]);
    close_fd(&c2_woke[0]);
    close_fd(&c2_done[1]);
    close_fd(&c1_park[0]);
    close(fd);
    result("setlkw-wakes-after-cascaded-downgrade", problem);
}

static volatile sig_atomic_t alarms;

static void on_alarm(int sig) {
    (void)sig;
    alarms++;
}

static void catch_alarm(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = on_alarm;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    sigaction(SIGALRM, &sa, NULL);
}

static int child_eintr(int unused) {
    (void)unused;
    int fd = open_file();
    catch_alarm();
    alarm(1);
    double start = now();
    int err = range_lock(fd, F_SETLKW, F_WRLCK, SEEK_SET, 0, 1);
    double waited = now() - start;
    signal(SIGALRM, SIG_DFL);
    alarm(20);
    if (err != EINTR) {
        printf("lock-probe: F_SETLKW during a signal returned %s\n", err ? strerror(err) : "success");
        return 1;
    }
    if (alarms != 1 || waited < 0.5 || waited > 8) {
        printf("lock-probe: EINTR after %.2fs with %d alarms\n", waited, (int)alarms);
        return 2;
    }
    return 0;
}

static void case_eintr(void) {
    fresh("eintr");
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 1) != 0) {
        result("setlkw-eintr", "F_SETLK failed");
    } else {
        result("setlkw-eintr", child_status(run(child_eintr, 0, 15)));
    }
    close(fd);
}

static int child_deadlock(int unused) {
    (void)unused;
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 1, 1) != 0) {
        return 1;
    }
    post(up[1]);
    if (range_lock(fd, F_SETLKW, F_WRLCK, SEEK_SET, 0, 1) != 0) {
        return 2;
    }
    setlk(fd, F_UNLCK, 0, 0);
    return 0;
}

static void case_deadlock(void) {
    fresh("deadlock");
    int fd = open_file();
    if (setlk(fd, F_WRLCK, 0, 1) != 0) {
        result("setlkw-edeadlk", "F_SETLK failed");
        close(fd);
        return;
    }
    struct child c;
    if (spawn_child(child_deadlock, 0, 1, &c) != 0) {
        setlk(fd, F_UNLCK, 0, 0);
        result("setlkw-edeadlk", "fork failed");
        close(fd);
        return;
    }
    enum ready r = await_ready(&c, READY_LIMIT_MS);
    if (r != READY) {
        setlk(fd, F_UNLCK, 0, 0);
        reap(c.pid, 10);
        close_pipes();
        result("setlkw-edeadlk", ready_problem(r));
        close(fd);
        return;
    }
    sleep_ms(500);
    catch_alarm();
    alarms = 0;
    alarm(5);
    int err = range_lock(fd, F_SETLKW, F_WRLCK, SEEK_SET, 1, 1);
    alarm(0);
    signal(SIGALRM, SIG_DFL);
    setlk(fd, F_UNLCK, 0, 0);
    int status = reap(c.pid, 10);
    close_pipes();
    const char *problem = NULL;
    if (err != EDEADLK) {
        problem = describe("closing a two-process cycle returned %s", err ? strerror(err) : "success");
    } else {
        problem = child_status(status);
    }
    result("setlkw-edeadlk", problem);
    close(fd);
}

static int child_ofd(int stage) {
    int inherited = 3;
    int fd = open_file();
    if (stage == 0) {
        if (range_lock(inherited, F_OFD_SETLK, F_WRLCK, SEEK_SET, 0, 10) != 0) {
            return 1;
        }
        if (!busy(range_lock(fd, F_OFD_SETLK, F_WRLCK, SEEK_SET, 0, 1))) {
            return 2;
        }
        if (!busy(setlk(fd, F_WRLCK, 0, 1))) {
            return 3;
        }
        return 0;
    }
    if (stage == 1) {
        if (!busy(range_lock(fd, F_OFD_SETLK, F_WRLCK, SEEK_SET, 0, 1))) {
            return 4;
        }
        struct flock fl = getlk(fd, F_OFD_GETLK, F_WRLCK, 0, 0);
        if (fl.l_type != F_WRLCK || fl.l_pid != -1 || fl.l_start != 0 || fl.l_len != 10) {
            printf("lock-probe: ofd getlk saw type %d pid %d start %lld len %lld\n", fl.l_type,
                   fl.l_pid, (long long)fl.l_start, (long long)fl.l_len);
            return 5;
        }
        return 0;
    }
    return range_lock(fd, F_OFD_SETLK, F_WRLCK, SEEK_SET, 0, 1) == 0 ? 0 : 6;
}

static int child_process_fork(int parent) {
    int inherited = 3;
    if (!busy(setlk(inherited, F_WRLCK, 0, 1))) {
        return 1;
    }
    struct flock fl = getlk(inherited, F_GETLK, F_WRLCK, 0, 1);
    if (fl.l_type != F_WRLCK || fl.l_pid != parent) {
        return 2;
    }
    return 0;
}

static void case_ownership(void) {
    fresh("ofd");
    int fd = open_file();
    const char *problem = NULL;
    if (fd != 3 && dup2(fd, 3) != 3) {
        problem = "dup2 failed";
    } else if (fd != 3) {
        close(fd);
        fd = 3;
    }
    if (!problem && range_lock(fd, F_OFD_SETLK, F_WRLCK, SEEK_SET, 0, 10) != 0) {
        problem = describe("F_OFD_SETLK failed: %s", strerror(errno));
    }
    if (!problem) {
        problem = child_status(run(child_ofd, 0, 10));
    }
    result("ofd-shared-by-fork", problem);
    if (!problem) {
        problem = child_status(run(child_ofd, 1, 10));
        result("ofd-survives-child-exit", problem);
    }
    if (!problem) {
        int other = open_file();
        problem = busy(setlk(other, F_WRLCK, 0, 1)) ? NULL
                                                     : "a process lock did not conflict with its own OFD lock";
        close(other);
        result("ofd-conflicts-with-process-lock", problem);
    }
    close(fd);
    if (!problem) {
        result("ofd-released-on-close", child_status(run(child_ofd, 2, 10)));
    }

    fresh("process-fork");
    fd = open_file();
    problem = NULL;
    if (fd != 3 && dup2(fd, 3) != 3) {
        problem = "dup2 failed";
    } else if (fd != 3) {
        close(fd);
        fd = 3;
    }
    if (!problem && setlk(fd, F_WRLCK, 0, 10) != 0) {
        problem = "F_SETLK failed";
    }
    if (!problem) {
        problem = child_status(run(child_process_fork, (int)getpid(), 10));
    }
    result("process-lock-not-inherited", problem);
    close(fd);
}

static int child_flock(int stage) {
    int fd = open_file();
    switch (stage) {
    case 0: {
        int exclusive = flock_err(fd, LOCK_EX | LOCK_NB);
        int shared = flock_err(fd, LOCK_SH | LOCK_NB);
        if (exclusive != EWOULDBLOCK || shared != EWOULDBLOCK) {
            struct flock fl = getlk(fd, F_GETLK, F_WRLCK, 0, 0);
            printf("lock-probe: beside LOCK_EX, LOCK_EX|LOCK_NB gave %s and LOCK_SH|LOCK_NB gave %s; "
                   "F_GETLK sees type %d pid %d\n",
                   exclusive ? strerror(exclusive) : "success", shared ? strerror(shared) : "success",
                   fl.l_type, fl.l_pid);
            return 1;
        }
        return 0;
    }
    case 1:
        return flock_err(fd, LOCK_EX | LOCK_NB) == 0 ? 0 : 3;
    case 2:
        return flock_err(fd, LOCK_SH | LOCK_NB) == 0 ? 0 : 4;
    case 3: {
        post(up[1]);
        double start = now();
        if (flock_err(fd, LOCK_EX) != 0) {
            return 5;
        }
        return now() - start < 0.3 ? 6 : 0;
    }
    case 4:
        return flock_err(3, LOCK_EX | LOCK_NB) == 0 ? 0 : 7;
    case 5:
        if (setlk(fd, F_WRLCK, 0, 0) != 0) {
            return 8;
        }
        setlk(fd, F_UNLCK, 0, 0);
        return 0;
    case 6:
        if (flock_err(fd, LOCK_EX) != 0) {
            return 9;
        }
        close(fd);
        post(up[1]);
        await_byte(down[0]);
        return 0;
    }
    return 10;
}

static void case_flock(void) {
    fresh("flock-ex");
    int fd = open_file();
    const char *problem = NULL;
    if (flock_err(fd, LOCK_EX) != 0) {
        problem = "LOCK_EX failed";
    }
    if (!problem) {
        problem = child_status(run(child_flock, 0, 10));
    }
    if (!problem && flock_err(fd, LOCK_UN) != 0) {
        problem = "LOCK_UN failed";
    }
    if (!problem) {
        problem = child_status(run(child_flock, 1, 10));
    }
    result("flock-exclusive-nb", problem);
    close(fd);

    fresh("flock-sh");
    fd = open_file();
    problem = flock_err(fd, LOCK_SH) != 0 ? "LOCK_SH failed" : child_status(run(child_flock, 2, 10));
    if (!problem && run(child_flock, 1, 10) == 0) {
        problem = "LOCK_EX was granted beside a shared lock";
    }
    result("flock-shared", problem);
    close(fd);

    fresh("flock-wait");
    fd = open_file();
    if (flock_err(fd, LOCK_EX) != 0) {
        result("flock-wakes", "LOCK_EX failed");
    } else {
        struct child c;
        if (spawn_child(child_flock, 3, 1, &c) != 0) {
            result("flock-wakes", "fork failed");
        } else {
            enum ready r = await_ready(&c, READY_LIMIT_MS);
            if (r != READY) {
                flock(fd, LOCK_UN);
                reap(c.pid, 10);
                close_pipes();
                result("flock-wakes", ready_problem(r));
            } else {
                sleep_ms(1000);
                flock(fd, LOCK_UN);
                int status = reap(c.pid, 10);
                close_pipes();
                result("flock-wakes", status == 6 ? "LOCK_EX returned before the lock was released"
                                                  : child_status(status));
            }
        }
    }
    close(fd);

    fresh("flock-fork");
    fd = open_file();
    problem = NULL;
    if (fd != 3 && dup2(fd, 3) != 3) {
        problem = "dup2 failed";
    } else if (fd != 3) {
        close(fd);
        fd = 3;
    }
    if (!problem && flock_err(fd, LOCK_EX) != 0) {
        problem = "LOCK_EX failed";
    }
    if (!problem) {
        problem = child_status(run(child_flock, 4, 10));
    }
    if (!problem && run(child_flock, 1, 10) == 0) {
        problem = "the lock was lost when a child sharing the description exited";
    }
    close(fd);
    if (!problem && run(child_flock, 1, 10) != 0) {
        problem = "the lock outlived the last descriptor of its description";
    }
    result("flock-owned-by-description", problem);

    fresh("flock-fcntl");
    fd = open_file();
    problem = flock_err(fd, LOCK_EX) != 0 ? "LOCK_EX failed" : child_status(run(child_flock, 5, 10));
    if (!problem && (flock(fd, LOCK_UN) != 0 || setlk(fd, F_WRLCK, 0, 0) != 0)) {
        problem = "switching from flock to fcntl failed";
    }
    if (!problem && run(child_flock, 1, 10) != 0) {
        problem = "an fcntl lock blocked flock";
    }
    result("flock-independent-of-fcntl", problem);
    close(fd);

    fresh("flock-close");
    struct child c;
    if (spawn_child(child_flock, 6, 1, &c) != 0) {
        result("flock-release-on-close", "fork failed");
        return;
    }
    enum ready r = await_ready(&c, READY_LIMIT_MS);
    if (r != READY) {
        reap(c.pid, 10);
        close_pipes();
        result("flock-release-on-close", ready_problem(r));
        return;
    }
    fd = open_file();
    problem = flock_err(fd, LOCK_EX | LOCK_NB) == 0 ? NULL : "the lock outlived close() in a live process";
    if (post_parent(down[1]) != 0 && !problem) {
        problem = "failed to signal child";
    }
    int status = reap(c.pid, 10);
    close_pipes();
    result("flock-release-on-close", problem ? problem : child_status(status));
    close(fd);
}

static int child_whence(int unused) {
    (void)unused;
    int fd = open_file();
    struct flock fl = getlk(fd, F_GETLK, F_WRLCK, 0, 0);
    if (fl.l_type != F_WRLCK || fl.l_start != 50 || fl.l_len != 10) {
        return 1;
    }
    fl = getlk(fd, F_GETLK, F_WRLCK, 70, 0);
    if (fl.l_type != F_WRLCK || fl.l_start != 90 || fl.l_len != 10) {
        return 2;
    }
    return 0;
}

static void case_whence(void) {
    fresh("whence");
    int fd = open_file();
    char block[100];
    memset(block, 'x', sizeof block);
    const char *problem = NULL;
    if (write(fd, block, sizeof block) != (ssize_t)sizeof block || lseek(fd, 50, SEEK_SET) != 50) {
        problem = "write or lseek failed";
    } else if (range_lock(fd, F_SETLK, F_WRLCK, SEEK_CUR, 0, 10) != 0 ||
               range_lock(fd, F_SETLK, F_WRLCK, SEEK_END, -10, 10) != 0) {
        problem = "F_SETLK with SEEK_CUR or SEEK_END failed";
    } else {
        problem = child_status(run(child_whence, 0, 10));
    }
    result("seek-cur-and-end", problem);
    close(fd);
}

int main(void) {
    signal(SIGPIPE, SIG_IGN);
    setvbuf(stdout, NULL, _IOLBF, 0);
    case_conflict();
    case_getlk();
    case_shared();
    case_split();
    case_whence();
    case_close();
    case_path_lookups();
    case_exit();
    case_ownership();
    case_setlkw();
    case_setlkw_downgrade();
    case_setlkw_cascade();
    case_eintr();
    case_deadlock();
    case_flock();
    if (failures) {
        printf("lock-probe: FAIL %d checks\n", failures);
        return 1;
    }
    printf("lock-probe: PASS\n");
    return 0;
}
