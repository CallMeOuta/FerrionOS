#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    CASE_ALARM_SEC = 4,
    STOP_SYNC_MS = 150,
    STOP_CONT_CYCLES = 32,
    POLL_STEPS = 50,
    POLL_STEP_MS = 100,
    EINTR_RETRIES = 16,
    SENTINEL_STATUS = 0x5a5a5a5a,
};

static void sleep_ms(long ms) {
    struct timespec req;
    struct timespec rem;
    req.tv_sec = ms / 1000;
    req.tv_nsec = (ms % 1000) * 1000000L;
    for (int i = 0; i < EINTR_RETRIES; i++) {
        if (nanosleep(&req, &rem) == 0 || errno != EINTR) {
            return;
        }
        req = rem;
    }
}

static const char *status_kind(pid_t ret, int st, char *buf, size_t size) {
    if (ret < 0) {
        snprintf(buf, size, "ERR(%d)", errno);
        return buf;
    }
    if (ret == 0) {
        return "ZERO";
    }
    if (WIFEXITED(st)) {
        snprintf(buf, size, "EXITED(%d)", WEXITSTATUS(st));
        return buf;
    }
    if (WIFSIGNALED(st)) {
        snprintf(buf, size, "SIGNALED(%d)", WTERMSIG(st));
        return buf;
    }
    if (WIFSTOPPED(st)) {
        snprintf(buf, size, "STOPPED(%d)", WSTOPSIG(st));
        return buf;
    }
    if (WIFCONTINUED(st)) {
        return "CONTINUED";
    }
    snprintf(buf, size, "OTHER(0x%x)", (unsigned)st);
    return buf;
}

static int read_byte(int fd) {
    for (int i = 0; i < EINTR_RETRIES; i++) {
        char b = 0;
        ssize_t n = read(fd, &b, 1);
        if (n == 1) {
            return (int)(unsigned char)b;
        }
        if (n == 0 || errno != EINTR) {
            return -1;
        }
    }
    return -1;
}

static void write_byte(int fd, char b) {
    ssize_t unused = write(fd, &b, 1);
    (void)unused;
}

static void kill_and_reap(pid_t pid) {
    if (pid <= 0) {
        return;
    }
    kill(pid, SIGKILL);
    int st = 0;
    for (int i = 0; i < 20; i++) {
        pid_t r = waitpid(pid, &st, WNOHANG | WUNTRACED | WCONTINUED);
        if (r == pid && (WIFEXITED(st) || WIFSIGNALED(st))) {
            return;
        }
        if (r < 0 && errno == ECHILD) {
            return;
        }
        sleep_ms(20);
    }
}

static void noop_handler(int sig) {
    (void)sig;
}

static void case_stop_wnohang_no_wuntraced(void) {
    int p[2];
    if (pipe(p) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        write_byte(p[1], 'S');
        raise(SIGSTOP);
        _exit(0);
    }
    close(p[1]);
    read_byte(p[0]);
    close(p[0]);
    sleep_ms(STOP_SYNC_MS);

    int st = SENTINEL_STATUS;
    char b1[32], b2[32];
    pid_t r1 = waitpid(c, &st, WNOHANG);
    const char *s1 = status_kind(r1, st, b1, sizeof(b1));
    int st1_kept = (st == SENTINEL_STATUS);
    st = 0;
    pid_t r2 = waitpid(c, &st, WUNTRACED | WNOHANG);
    const char *s2 = status_kind(r2, st, b2, sizeof(b2));
    printf("r1=%s kept=%d r2=%s\n", s1, st1_kept, s2);
    kill_and_reap(c);
}

static void case_stop_wuntraced_then_wnohang(void) {
    int p[2];
    if (pipe(p) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        write_byte(p[1], 'S');
        raise(SIGSTOP);
        _exit(0);
    }
    close(p[1]);
    read_byte(p[0]);
    close(p[0]);

    int st = 0;
    char b1[32], b2[32];
    pid_t r1 = waitpid(c, &st, WUNTRACED);
    const char *s1 = status_kind(r1, st, b1, sizeof(b1));
    st = SENTINEL_STATUS;
    pid_t r2 = waitpid(c, &st, WNOHANG);
    const char *s2 = status_kind(r2, st, b2, sizeof(b2));
    int st2_kept = (st == SENTINEL_STATUS);
    printf("r1=%s r2=%s kept=%d\n", s1, s2, st2_kept);
    kill_and_reap(c);
}

static void case_stop_blocking_eintr_sentinel(void) {
    int p[2];
    if (pipe(p) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        write_byte(p[1], 'S');
        raise(SIGSTOP);
        _exit(0);
    }
    close(p[1]);
    read_byte(p[0]);
    close(p[0]);
    sleep_ms(STOP_SYNC_MS);

    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = noop_handler;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGUSR1, &sa, NULL);

    pid_t killer = fork();
    if (killer == 0) {
        sleep_ms(300);
        kill(getppid(), SIGUSR1);
        _exit(0);
    }

    int st = SENTINEL_STATUS;
    errno = 0;
    pid_t r1 = waitpid(c, &st, 0);
    int e1 = errno;
    int kept = (st == SENTINEL_STATUS);

    int kst = 0;
    waitpid(killer, &kst, 0);
    st = 0;
    char b2[32];
    pid_t r2 = waitpid(c, &st, WUNTRACED | WNOHANG);
    const char *s2 = status_kind(r2, st, b2, sizeof(b2));
    printf("r1=%d eintr=%d kept=%d r2=%s\n", (int)r1, e1 == EINTR ? 1 : 0, kept, s2);
    kill_and_reap(c);
}

static void case_cont_wnohang_and_wcontinued(void) {
    int p[2];
    int go[2];
    if (pipe(p) != 0 || pipe(go) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        close(go[1]);
        raise(SIGSTOP);
        write_byte(p[1], 'C');
        read_byte(go[0]);
        close(go[0]);
        _exit(0);
    }
    close(p[1]);
    close(go[0]);

    int st = 0;
    char b1[32], b2[32], b3[32];
    pid_t r1 = waitpid(c, &st, WUNTRACED);
    const char *s1 = status_kind(r1, st, b1, sizeof(b1));
    kill(c, SIGCONT);
    read_byte(p[0]);
    close(p[0]);

    st = SENTINEL_STATUS;
    pid_t r2 = waitpid(c, &st, WNOHANG);
    const char *s2 = status_kind(r2, st, b2, sizeof(b2));
    int kept = (st == SENTINEL_STATUS);
    st = 0;
    pid_t r3 = waitpid(c, &st, WCONTINUED | WNOHANG);
    const char *s3 = status_kind(r3, st, b3, sizeof(b3));
    printf("r1=%s r2=%s kept=%d r3=%s\n", s1, s2, kept, s3);
    close(go[1]);
    kill_and_reap(c);
}

static void case_cont_then_blocking_exit(void) {
    int p[2];
    if (pipe(p) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        raise(SIGSTOP);
        write_byte(p[1], 'C');
        sleep_ms(STOP_SYNC_MS);
        _exit(42);
    }
    close(p[1]);

    int st = 0;
    char b1[32], b2[32];
    pid_t r1 = waitpid(c, &st, WUNTRACED);
    const char *s1 = status_kind(r1, st, b1, sizeof(b1));
    kill(c, SIGCONT);
    read_byte(p[0]);
    close(p[0]);

    st = 0;
    pid_t r2 = waitpid(c, &st, 0);
    const char *s2 = status_kind(r2, st, b2, sizeof(b2));
    printf("r1=%s r2=%s\n", s1, s2);
}

static void case_target_any_child(void) {
    int p[2];
    int go[2];
    if (pipe(p) != 0 || pipe(go) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        close(go[1]);
        write_byte(p[1], 'S');
        raise(SIGSTOP);
        write_byte(p[1], 'C');
        read_byte(go[0]);
        close(go[0]);
        _exit(7);
    }
    close(p[1]);
    close(go[0]);
    read_byte(p[0]);
    sleep_ms(STOP_SYNC_MS);

    int st = 0;
    char b1[32], b2[32], b3[32], b4[32], b5[32];
    pid_t r1 = waitpid(-1, &st, WNOHANG);
    const char *s1 = status_kind(r1, st, b1, sizeof(b1));
    st = 0;
    pid_t r2 = waitpid(-1, &st, WUNTRACED);
    const char *s2 = status_kind(r2, st, b2, sizeof(b2));
    kill(c, SIGCONT);
    read_byte(p[0]);
    close(p[0]);
    st = 0;
    pid_t r3 = waitpid(-1, &st, WNOHANG);
    const char *s3 = status_kind(r3, st, b3, sizeof(b3));
    st = 0;
    pid_t r4 = waitpid(-1, &st, WCONTINUED | WNOHANG);
    const char *s4 = status_kind(r4, st, b4, sizeof(b4));
    close(go[1]);
    st = 0;
    pid_t r5 = waitpid(-1, &st, 0);
    const char *s5 = status_kind(r5, st, b5, sizeof(b5));
    printf("r1=%s r2=%s r3=%s r4=%s r5=%s\n", s1, s2, s3, s4, s5);
}

static void case_target_pgid(void) {
    int p[2];
    int go[2];
    if (pipe(p) != 0 || pipe(go) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        close(go[1]);
        setpgid(0, 0);
        write_byte(p[1], 'S');
        raise(SIGSTOP);
        write_byte(p[1], 'C');
        read_byte(go[0]);
        close(go[0]);
        _exit(9);
    }
    close(p[1]);
    close(go[0]);
    read_byte(p[0]);
    setpgid(c, c);
    sleep_ms(STOP_SYNC_MS);

    pid_t pg = -c;
    int st = 0;
    char b1[32], b2[32], b3[32], b4[32], b5[32];
    pid_t r1 = waitpid(pg, &st, WNOHANG);
    const char *s1 = status_kind(r1, st, b1, sizeof(b1));
    st = 0;
    pid_t r2 = waitpid(pg, &st, WUNTRACED);
    const char *s2 = status_kind(r2, st, b2, sizeof(b2));
    kill(c, SIGCONT);
    read_byte(p[0]);
    close(p[0]);
    st = 0;
    pid_t r3 = waitpid(pg, &st, WNOHANG);
    const char *s3 = status_kind(r3, st, b3, sizeof(b3));
    st = 0;
    pid_t r4 = waitpid(pg, &st, WCONTINUED | WNOHANG);
    const char *s4 = status_kind(r4, st, b4, sizeof(b4));
    close(go[1]);
    st = 0;
    pid_t r5 = waitpid(pg, &st, 0);
    const char *s5 = status_kind(r5, st, b5, sizeof(b5));
    printf("r1=%s r2=%s r3=%s r4=%s r5=%s\n", s1, s2, s3, s4, s5);
}

static void case_stop_cont_cycles_then_exit(void) {
    int p[2];
    if (pipe(p) != 0) {
        printf("pipe_err\n");
        _exit(1);
    }
    pid_t c = fork();
    if (c == 0) {
        close(p[0]);
        for (int i = 0; i < STOP_CONT_CYCLES; i++) {
            write_byte(p[1], 'S');
            raise(SIGSTOP);
            write_byte(p[1], 'C');
        }
        _exit(13);
    }
    close(p[1]);

    for (int i = 0; i < STOP_CONT_CYCLES; i++) {
        read_byte(p[0]);
        int st = 0;
        if (waitpid(c, &st, WUNTRACED) != c || !WIFSTOPPED(st)) {
            printf("cycle_stop_err=%d\n", i);
            kill_and_reap(c);
            return;
        }
        kill(c, SIGCONT);
        read_byte(p[0]);
    }
    close(p[0]);

    int st = 0;
    char b1[32], b2[32];
    pid_t r1 = waitpid(c, &st, WNOHANG);
    const char *s1 = status_kind(r1, st, b1, sizeof(b1));
    if (r1 == 0) {
        st = 0;
        pid_t r2 = waitpid(c, &st, 0);
        const char *s2 = status_kind(r2, st, b2, sizeof(b2));
        printf("cycles=%d exit=%s\n", STOP_CONT_CYCLES, s2);
    } else {
        printf("cycles=%d exit=%s\n", STOP_CONT_CYCLES, s1);
    }
}

struct waitpid_case {
    const char *name;
    void (*fn)(void);
};

static int run_case(const struct waitpid_case *tc) {
    int out[2];
    if (pipe(out) != 0) {
        printf("case %s PIPE_ERR\n", tc->name);
        return 1;
    }
    fflush(stdout);
    pid_t worker = fork();
    if (worker == 0) {
        close(out[0]);
        dup2(out[1], STDOUT_FILENO);
        close(out[1]);
        alarm(CASE_ALARM_SEC);
        tc->fn();
        fflush(stdout);
        _exit(0);
    }
    close(out[1]);
    fcntl(out[0], F_SETFL, O_NONBLOCK);

    int st = 0;
    int finished = 0;
    for (int i = 0; i < POLL_STEPS; i++) {
        pid_t r = waitpid(worker, &st, WNOHANG);
        if (r == worker) {
            finished = 1;
            break;
        }
        sleep_ms(POLL_STEP_MS);
    }
    if (!finished) {
        kill(worker, SIGKILL);
        waitpid(worker, &st, 0);
        close(out[0]);
        printf("case %s HANG\n", tc->name);
        return 1;
    }

    char buf[256];
    ssize_t n = read(out[0], buf, sizeof(buf) - 1);
    close(out[0]);
    if (n < 0) {
        n = 0;
    }
    buf[n] = '\0';
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == '\r')) {
        buf[--n] = '\0';
    }
    if (!WIFEXITED(st) || WEXITSTATUS(st) != 0) {
        char kbuf[32];
        printf("case %s CRASH(%s) %s\n", tc->name, status_kind(worker, st, kbuf, sizeof(kbuf)), buf);
        return 1;
    }
    printf("case %s %s\n", tc->name, buf);
    return 0;
}

int main(void) {
    static const struct waitpid_case cases[] = {
        {"stop_wnohang_no_wuntraced", case_stop_wnohang_no_wuntraced},
        {"stop_wuntraced_then_wnohang", case_stop_wuntraced_then_wnohang},
        {"stop_blocking_eintr_sentinel", case_stop_blocking_eintr_sentinel},
        {"cont_wnohang_and_wcontinued", case_cont_wnohang_and_wcontinued},
        {"cont_then_blocking_exit", case_cont_then_blocking_exit},
        {"target_any_child", case_target_any_child},
        {"target_pgid", case_target_pgid},
        {"stop_cont_cycles_then_exit", case_stop_cont_cycles_then_exit},
    };
    const size_t count = sizeof(cases) / sizeof(cases[0]);
    int failed = 0;
    for (size_t i = 0; i < count; i++) {
        failed += run_case(&cases[i]);
    }
    if (failed != 0) {
        printf("waitpid-probe: FAIL %d\n", failed);
        return 1;
    }
    printf("waitpid-probe: DONE\n");
    return 0;
}
