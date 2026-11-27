#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define PROBE_PATH "/usr/bin/exec-handles-probe"
#define CHECKER_PATH "/usr/bin/exec-handles-checker"
#define WORKER_THREADS 8
#define SINGLE_ROUNDS 3
#define MULTI_ROUNDS 20
#define ACTIVE_ROUNDS 3
#define MAX_ROUNDS 64
#define MAX_WAIT_ITERS 64
#define MAX_COND_WAITS 1024
#define MAX_DIRECT_SLEEPS 2000
#define DIRECT_SLEEP_NS 5000000L
#define ACTIVE_MIN_LOOPS 5
#define ACTIVE_SIBLING_ITERS 4000
#define ACTIVE_SLEEP_NS 5000000L
#define BLOCKED_SETTLE_NS 20000000L
#define BLOCKED_SLEEP_SEC 30
#define BLOCKED_SIBLINGS 4
#define FORK_EXEC_ROUNDS 40
#define TWO_FORKER_THREADS 2
#define FORK_LOOP_MAX_ITERS 2000
#define FORK_JITTER_STEPS 8
#define FORK_JITTER_STEP_NS 200000L
#define PROBE_HOLD_MS 2000
#define HOLD_STEP_MS 100
#define HOLD_MAX_STEPS (PROBE_HOLD_MS / HOLD_STEP_MS)
#define EXIT_FORK_ROUNDS 40
#define MAX_EXIT_FAILS_PER_TRIGGER 3
#define EXIT_READY_TIMEOUT_MS 5000
#define EXIT_EOF_TIMEOUT_MS 5000
#define EXIT_KILL_SLEEP_NS 50000000L
#define EXIT_KILL_MAX_SLEEPS 200
#define EXIT_PIPE_FD 3
#define EXEC_TIMING_CHILDREN 100
#define EXEC_TIMING_ROUNDS 5
#define EXEC_TIMING_BUF_CAP 256

extern char **environ;

struct worker_state {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int ready;
    int stop;
};

struct active_worker_arg {
    struct worker_state *st;
    const char *marker_path;
    int id;
};

struct blocked_worker_arg {
    struct worker_state *st;
    int read_fd;
};

struct cond_blocked_arg {
    struct worker_state *st;
    pthread_mutex_t mu;
    pthread_cond_t cv;
};

static uint64_t now_us(void)
{
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000000ULL + (uint64_t)(ts.tv_nsec / 1000L);
}

static int cmp_u64(const void *a, const void *b)
{
    uint64_t ua = *(const uint64_t *)a;
    uint64_t ub = *(const uint64_t *)b;
    return (ua > ub) - (ua < ub);
}

static void *worker_fn(void *arg)
{
    struct worker_state *st = (struct worker_state *)arg;
    pthread_mutex_lock(&st->mu);
    st->ready++;
    pthread_cond_broadcast(&st->cv);
    for (int i = 0; i < MAX_COND_WAITS && !st->stop; i++) {
        pthread_cond_wait(&st->cv, &st->mu);
    }
    assert(st->stop);
    pthread_mutex_unlock(&st->mu);
    return NULL;
}

static void *direct_worker_fn(void *arg)
{
    struct worker_state *st = (struct worker_state *)arg;
    pthread_mutex_lock(&st->mu);
    st->ready++;
    pthread_cond_broadcast(&st->cv);
    pthread_mutex_unlock(&st->mu);

    struct timespec req;
    req.tv_sec = 0;
    req.tv_nsec = DIRECT_SLEEP_NS;
    for (int i = 0; i < MAX_DIRECT_SLEEPS; i++) {
        (void)nanosleep(&req, NULL);
    }
    return NULL;
}

static void *active_worker_fn(void *arg)
{
    struct active_worker_arg *wa = (struct active_worker_arg *)arg;
    int signalled = 0;
    struct timespec req;
    req.tv_sec = 0;
    req.tv_nsec = ACTIVE_SLEEP_NS;

    for (int i = 0; i < ACTIVE_SIBLING_ITERS; i++) {
        int mfd = open(wa->marker_path, O_WRONLY | O_APPEND | O_CLOEXEC);
        if (mfd >= 0) {
            char rec[32];
            int len = snprintf(rec, sizeof(rec), "%d:%d\n", wa->id, i);
            if (len > 0) {
                (void)write(mfd, rec, (size_t)len);
            }
            (void)close(mfd);
        }
        int nfd = open("/dev/null", O_RDONLY | O_CLOEXEC);
        if (nfd >= 0) {
            (void)close(nfd);
        }
        if (!signalled && i + 1 >= ACTIVE_MIN_LOOPS) {
            signalled = 1;
            pthread_mutex_lock(&wa->st->mu);
            wa->st->ready++;
            pthread_cond_broadcast(&wa->st->cv);
            pthread_mutex_unlock(&wa->st->mu);
        }
        (void)nanosleep(&req, NULL);
    }
    return NULL;
}

static void *blocked_worker_fn(void *arg)
{
    struct blocked_worker_arg *ba = (struct blocked_worker_arg *)arg;
    pthread_mutex_lock(&ba->st->mu);
    ba->st->ready++;
    pthread_cond_broadcast(&ba->st->cv);
    pthread_mutex_unlock(&ba->st->mu);

    char byte = 0;
    for (int i = 0; i < MAX_WAIT_ITERS; i++) {
        ssize_t r = read(ba->read_fd, &byte, 1);
        if (r < 0 && errno == EINTR) {
            continue;
        }
        break;
    }
    return NULL;
}

static void *cond_blocked_worker_fn(void *arg)
{
    struct cond_blocked_arg *ca = (struct cond_blocked_arg *)arg;
    pthread_mutex_lock(&ca->mu);
    pthread_mutex_lock(&ca->st->mu);
    ca->st->ready++;
    pthread_cond_broadcast(&ca->st->cv);
    pthread_mutex_unlock(&ca->st->mu);
    for (int i = 0; i < MAX_WAIT_ITERS; i++) {
        (void)pthread_cond_wait(&ca->cv, &ca->mu);
    }
    pthread_mutex_unlock(&ca->mu);
    return NULL;
}

static void *sleep_blocked_worker_fn(void *arg)
{
    struct worker_state *st = (struct worker_state *)arg;
    pthread_mutex_lock(&st->mu);
    st->ready++;
    pthread_cond_broadcast(&st->cv);
    pthread_mutex_unlock(&st->mu);

    struct timespec req;
    req.tv_sec = BLOCKED_SLEEP_SEC;
    req.tv_nsec = 0;
    for (int i = 0; i < MAX_WAIT_ITERS; i++) {
        if (nanosleep(&req, NULL) == 0) {
            break;
        }
        if (errno != EINTR) {
            break;
        }
    }
    return NULL;
}

static int direct_exec_helper(const char *threads_str, const char *verbose_str)
{
    struct worker_state st;
    int rc = pthread_mutex_init(&st.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&st.cv, NULL);
    assert(rc == 0);
    st.ready = 0;
    st.stop = 0;

    pthread_t workers[WORKER_THREADS];
    for (int i = 0; i < WORKER_THREADS; i++) {
        rc = pthread_create(&workers[i], NULL, direct_worker_fn, &st);
        assert(rc == 0);
    }

    pthread_mutex_lock(&st.mu);
    for (int i = 0; i < MAX_COND_WAITS && st.ready < WORKER_THREADS; i++) {
        pthread_cond_wait(&st.cv, &st.mu);
    }
    assert(st.ready == WORKER_THREADS);
    pthread_mutex_unlock(&st.mu);

    char *argv[] = {
        (char *)CHECKER_PATH,
        (char *)"direct-exec",
        (char *)threads_str,
        (char *)verbose_str,
        NULL
    };
    execve(CHECKER_PATH, argv, environ);
    fprintf(stderr, "exec-handles-probe: direct execve failed errno=%d\n", errno);
    _exit(127);
}

static int direct_exec_active_helper(const char *threads_str, const char *verbose_str, const char *marker_path)
{
    int pfd[2];
    int rc = pipe(pfd);
    assert(rc == 0);

    int sv[2];
    rc = socketpair(AF_UNIX, SOCK_STREAM, 0, sv);
    assert(rc == 0);

    struct worker_state st;
    rc = pthread_mutex_init(&st.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&st.cv, NULL);
    assert(rc == 0);
    st.ready = 0;
    st.stop = 0;

    struct active_worker_arg args[WORKER_THREADS];
    pthread_t workers[WORKER_THREADS];
    for (int i = 0; i < WORKER_THREADS; i++) {
        args[i].st = &st;
        args[i].marker_path = marker_path;
        args[i].id = i;
        rc = pthread_create(&workers[i], NULL, active_worker_fn, &args[i]);
        assert(rc == 0);
    }

    struct blocked_worker_arg pipe_arg;
    pipe_arg.st = &st;
    pipe_arg.read_fd = pfd[0];
    pthread_t pipe_thr;
    rc = pthread_create(&pipe_thr, NULL, blocked_worker_fn, &pipe_arg);
    assert(rc == 0);

    struct blocked_worker_arg sock_arg;
    sock_arg.st = &st;
    sock_arg.read_fd = sv[0];
    pthread_t sock_thr;
    rc = pthread_create(&sock_thr, NULL, blocked_worker_fn, &sock_arg);
    assert(rc == 0);

    struct cond_blocked_arg cond_arg;
    cond_arg.st = &st;
    rc = pthread_mutex_init(&cond_arg.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&cond_arg.cv, NULL);
    assert(rc == 0);
    pthread_t cond_thr;
    rc = pthread_create(&cond_thr, NULL, cond_blocked_worker_fn, &cond_arg);
    assert(rc == 0);

    pthread_t sleep_thr;
    rc = pthread_create(&sleep_thr, NULL, sleep_blocked_worker_fn, &st);
    assert(rc == 0);

    const int expected_ready = WORKER_THREADS + BLOCKED_SIBLINGS;
    pthread_mutex_lock(&st.mu);
    for (int i = 0; i < MAX_COND_WAITS && st.ready < expected_ready; i++) {
        pthread_cond_wait(&st.cv, &st.mu);
    }
    assert(st.ready == expected_ready);
    pthread_mutex_unlock(&st.mu);

    struct timespec settle;
    settle.tv_sec = 0;
    settle.tv_nsec = BLOCKED_SETTLE_NS;
    (void)nanosleep(&settle, NULL);

    char *argv[] = {
        (char *)CHECKER_PATH,
        (char *)"direct-exec-active",
        (char *)threads_str,
        (char *)verbose_str,
        (char *)marker_path,
        NULL
    };
    execve(CHECKER_PATH, argv, environ);
    fprintf(stderr, "exec-handles-probe: direct-exec-active execve failed errno=%d\n", errno);
    _exit(127);
}

static int wait_child(pid_t pid, int *status_out)
{
    for (int i = 0; i < MAX_WAIT_ITERS; i++) {
        int st = 0;
        pid_t r = waitpid(pid, &st, 0);
        if (r == pid) {
            *status_out = st;
            return 0;
        }
        if (r < 0 && errno == EINTR) {
            continue;
        }
        return -1;
    }
    return -1;
}

static int run_fork_exec(const char *mode, const char *threads_str, const char *verbose_str, uint64_t *elapsed_us)
{
    char *argv[] = {
        (char *)CHECKER_PATH,
        (char *)mode,
        (char *)threads_str,
        (char *)verbose_str,
        NULL
    };
    uint64_t t0 = now_us();
    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "exec-handles-probe: fork failed errno=%d\n", errno);
        return -1;
    }
    if (pid == 0) {
        execve(CHECKER_PATH, argv, environ);
        _exit(127);
    }
    int st = 0;
    if (wait_child(pid, &st) != 0) {
        fprintf(stderr, "exec-handles-probe: waitpid failed errno=%d\n", errno);
        return -1;
    }
    *elapsed_us = now_us() - t0;
    if (!WIFEXITED(st)) {
        fprintf(stderr, "exec-handles-probe: child abnormal status=0x%x\n", st);
        return 1;
    }
    return WEXITSTATUS(st);
}

static int run_posix_spawn(const char *mode, const char *threads_str, const char *verbose_str, uint64_t *elapsed_us)
{
    char *argv[] = {
        (char *)CHECKER_PATH,
        (char *)mode,
        (char *)threads_str,
        (char *)verbose_str,
        NULL
    };
    uint64_t t0 = now_us();
    pid_t pid = 0;
    int rc = posix_spawn(&pid, CHECKER_PATH, NULL, NULL, argv, environ);
    if (rc != 0) {
        fprintf(stderr, "exec-handles-probe: posix_spawn failed errno=%d\n", rc);
        return -1;
    }
    int st = 0;
    if (wait_child(pid, &st) != 0) {
        fprintf(stderr, "exec-handles-probe: waitpid failed errno=%d\n", errno);
        return -1;
    }
    *elapsed_us = now_us() - t0;
    if (!WIFEXITED(st)) {
        fprintf(stderr, "exec-handles-probe: spawned child abnormal status=0x%x\n", st);
        return 1;
    }
    return WEXITSTATUS(st);
}

static int run_direct_exec(const char *threads_str, const char *verbose_str, uint64_t *elapsed_us)
{
    char *argv[] = {
        (char *)PROBE_PATH,
        (char *)"--direct-exec-helper",
        (char *)threads_str,
        (char *)verbose_str,
        NULL
    };
    uint64_t t0 = now_us();
    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "exec-handles-probe: direct-exec fork failed errno=%d\n", errno);
        return -1;
    }
    if (pid == 0) {
        execve(PROBE_PATH, argv, environ);
        _exit(127);
    }
    int st = 0;
    if (wait_child(pid, &st) != 0) {
        fprintf(stderr, "exec-handles-probe: direct-exec waitpid failed errno=%d\n", errno);
        return -1;
    }
    *elapsed_us = now_us() - t0;
    if (!WIFEXITED(st)) {
        fprintf(stderr, "exec-handles-probe: direct-exec abnormal status=0x%x\n", st);
        return 1;
    }
    return WEXITSTATUS(st);
}

static int run_direct_exec_active(const char *threads_str, const char *verbose_str, int round_idx, uint64_t *elapsed_us)
{
    char marker_path[96];
    snprintf(marker_path, sizeof(marker_path), "/tmp/eh-sib-%ld-%d.log", (long)getpid(), round_idx);
    int fd = open(marker_path, O_CREAT | O_TRUNC | O_WRONLY | O_CLOEXEC, 0600);
    if (fd < 0) {
        fprintf(stderr, "exec-handles-probe: create marker failed errno=%d\n", errno);
        return -1;
    }
    (void)close(fd);

    char *argv[] = {
        (char *)PROBE_PATH,
        (char *)"--direct-exec-active-helper",
        (char *)threads_str,
        (char *)verbose_str,
        marker_path,
        NULL
    };
    uint64_t t0 = now_us();
    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "exec-handles-probe: direct-exec-active fork failed errno=%d\n", errno);
        (void)unlink(marker_path);
        return -1;
    }
    if (pid == 0) {
        execve(PROBE_PATH, argv, environ);
        _exit(127);
    }
    int st = 0;
    if (wait_child(pid, &st) != 0) {
        fprintf(stderr, "exec-handles-probe: direct-exec-active waitpid failed errno=%d\n", errno);
        (void)unlink(marker_path);
        return -1;
    }
    *elapsed_us = now_us() - t0;
    (void)unlink(marker_path);
    if (!WIFEXITED(st)) {
        fprintf(stderr, "exec-handles-probe: direct-exec-active abnormal status=0x%x\n", st);
        return 1;
    }
    return WEXITSTATUS(st);
}

static int run_case(const char *case_name, const char *mode, int mode_kind, int nthreads, int rounds)
{
    assert(rounds > 0 && rounds <= MAX_ROUNDS);
    char threads_str[16];
    snprintf(threads_str, sizeof(threads_str), "%d", nthreads);

    uint64_t times[MAX_ROUNDS];
    int completed = 0;
    int failed = 0;
    int last_fail_rc = 0;
    for (int i = 0; i < rounds; i++) {
        const char *verbose_str = (i == 0) ? "1" : "0";
        uint64_t dt = 0;
        int rc;
        if (mode_kind == 0) {
            rc = run_fork_exec(mode, threads_str, verbose_str, &dt);
        } else if (mode_kind == 1) {
            rc = run_posix_spawn(mode, threads_str, verbose_str, &dt);
        } else if (mode_kind == 2) {
            rc = run_direct_exec(threads_str, verbose_str, &dt);
        } else {
            rc = run_direct_exec_active(threads_str, verbose_str, i, &dt);
        }
        if (rc != 0) {
            failed++;
            last_fail_rc = rc;
        }
        if (rc >= 0) {
            times[completed++] = dt;
        }
    }

    uint64_t median_us = 0;
    if (completed > 0) {
        qsort(times, (size_t)completed, sizeof(uint64_t), cmp_u64);
        median_us = times[completed / 2];
    }

    fprintf(stdout, "exec-handles-probe: case=%s threads=%d rounds=%d failed=%d last_rc=%d median_us=%llu\n",
        mode, nthreads, rounds, failed, last_fail_rc, (unsigned long long)median_us);
    if (failed == 0) {
        fprintf(stdout, "exec-handles-probe: pass %s\n", case_name);
    }
    fflush(stdout);
    return failed;
}

static void *fork_loop_worker_fn(void *arg)
{
    struct worker_state *st = (struct worker_state *)arg;
    for (int i = 0; i < FORK_LOOP_MAX_ITERS; i++) {
        pid_t child = fork();
        if (child == 0) {
            _exit(0);
        }
        if (child > 0) {
            int cst = 0;
            for (int w = 0; w < MAX_WAIT_ITERS; w++) {
                pid_t r = waitpid(child, &cst, 0);
                if (r == child) {
                    break;
                }
                if (r < 0 && errno != EINTR) {
                    break;
                }
            }
        }
        if (i == 0) {
            pthread_mutex_lock(&st->mu);
            st->ready++;
            pthread_cond_broadcast(&st->cv);
            pthread_mutex_unlock(&st->mu);
        }
    }
    return NULL;
}

static int exec_during_fork_helper_n(const char *round_str, int forker_count)
{
    assert(forker_count >= 1 && forker_count <= TWO_FORKER_THREADS);
    int round_idx = atoi(round_str);
    struct worker_state st;
    int rc = pthread_mutex_init(&st.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&st.cv, NULL);
    assert(rc == 0);
    st.ready = 0;
    st.stop = 0;

    pthread_t forkers[TWO_FORKER_THREADS];
    for (int f = 0; f < forker_count; f++) {
        rc = pthread_create(&forkers[f], NULL, fork_loop_worker_fn, &st);
        assert(rc == 0);
    }

    pthread_mutex_lock(&st.mu);
    for (int i = 0; i < MAX_COND_WAITS && st.ready < forker_count; i++) {
        pthread_cond_wait(&st.cv, &st.mu);
    }
    assert(st.ready == forker_count);
    pthread_mutex_unlock(&st.mu);

    long jitter_ns = (long)(round_idx % FORK_JITTER_STEPS) * FORK_JITTER_STEP_NS;
    if (jitter_ns > 0) {
        struct timespec req;
        req.tv_sec = 0;
        req.tv_nsec = jitter_ns;
        (void)nanosleep(&req, NULL);
    }

    char *argv[] = {
        (char *)CHECKER_PATH,
        (char *)"--fork-orphan-check",
        (char *)round_str,
        NULL
    };
    execve(CHECKER_PATH, argv, environ);
    fprintf(stderr, "exec-handles-probe: exec-during-fork execve failed errno=%d\n", errno);
    _exit(127);
}

static void hold_for_control(void)
{
    struct timespec step_ts;
    step_ts.tv_sec = 0;
    step_ts.tv_nsec = HOLD_STEP_MS * 1000000L;
    for (int s = 0; s < HOLD_MAX_STEPS; s++) {
        (void)nanosleep(&step_ts, NULL);
    }
}

static int run_exec_during_fork_suite_mode(const char *case_name, const char *helper_flag)
{
    int failed = 0;
    int orphan_hits = 0;
    int last_fail_rc = 0;
    for (int i = 0; i < FORK_EXEC_ROUNDS; i++) {
        char round_str[16];
        snprintf(round_str, sizeof(round_str), "%d", i);
        char *argv[] = {
            (char *)PROBE_PATH,
            (char *)helper_flag,
            round_str,
            NULL
        };
        pid_t pid = fork();
        if (pid < 0) {
            fprintf(stderr, "exec-handles-probe: %s fork failed round=%d errno=%d\n", case_name, i, errno);
            failed++;
            last_fail_rc = -1;
            continue;
        }
        if (pid == 0) {
            execve(PROBE_PATH, argv, environ);
            _exit(127);
        }
        int st = 0;
        if (wait_child(pid, &st) != 0) {
            fprintf(stderr, "exec-handles-probe: %s waitpid failed round=%d errno=%d\n", case_name, i, errno);
            failed++;
            last_fail_rc = -1;
            continue;
        }
        int rc = WIFEXITED(st) ? WEXITSTATUS(st) : 128 + WTERMSIG(st);
        if (rc != 0) {
            failed++;
            last_fail_rc = rc;
            if (rc == 47) {
                orphan_hits++;
            }
        }
    }
    fprintf(stdout, "exec-handles-probe: case=%s rounds=%d failed=%d orphan_hits=%d last_rc=%d\n",
        case_name, FORK_EXEC_ROUNDS, failed, orphan_hits, last_fail_rc);
    fflush(stdout);
    hold_for_control();
    if (failed == 0) {
        fprintf(stdout, "exec-handles-probe: pass %s\n", case_name);
        fprintf(stdout, "exec-handles-probe: PASS\n");
        fflush(stdout);
        return 0;
    }
    fprintf(stdout, "exec-handles-probe: FAIL total_failures=%d\n", failed);
    fflush(stdout);
    return 1;
}

static int exit_during_fork_helper(const char *mode, const char *round_str)
{
    int round_idx = atoi(round_str);
    struct worker_state st;
    int rc = pthread_mutex_init(&st.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&st.cv, NULL);
    assert(rc == 0);
    st.ready = 0;
    st.stop = 0;

    pthread_t forker;
    rc = pthread_create(&forker, NULL, fork_loop_worker_fn, &st);
    assert(rc == 0);

    pthread_mutex_lock(&st.mu);
    for (int i = 0; i < MAX_COND_WAITS && st.ready == 0; i++) {
        pthread_cond_wait(&st.cv, &st.mu);
    }
    assert(st.ready == 1);
    pthread_mutex_unlock(&st.mu);

    char ready_byte = 'R';
    ssize_t wr = write(EXIT_PIPE_FD, &ready_byte, 1);
    assert(wr == 1);

    if (strcmp(mode, "exit") == 0) {
        long jitter_ns = (long)(round_idx % FORK_JITTER_STEPS) * FORK_JITTER_STEP_NS;
        if (jitter_ns > 0) {
            struct timespec req;
            req.tv_sec = 0;
            req.tv_nsec = jitter_ns;
            (void)nanosleep(&req, NULL);
        }
        _exit(0);
    }

    struct timespec wait_ts;
    wait_ts.tv_sec = 0;
    wait_ts.tv_nsec = EXIT_KILL_SLEEP_NS;
    for (int i = 0; i < EXIT_KILL_MAX_SLEEPS; i++) {
        (void)nanosleep(&wait_ts, NULL);
    }
    _exit(99);
}

static int run_exit_during_fork_trigger(const char *trigger)
{
    int is_kill = (strcmp(trigger, "kill") == 0);
    int rounds_run = 0;
    int failed = 0;
    int eof_timeouts = 0;

    for (int i = 0; i < EXIT_FORK_ROUNDS; i++) {
        rounds_run++;
        int pfd[2];
        if (pipe(pfd) != 0) {
            fprintf(stdout, "exec-handles-probe: exit-during-fork pipe failed trigger=%s round=%d errno=%d\n",
                trigger, i, errno);
            failed++;
            if (failed >= MAX_EXIT_FAILS_PER_TRIGGER) {
                break;
            }
            continue;
        }
        int rflags = fcntl(pfd[0], F_GETFD, 0);
        assert(rflags >= 0);
        int frc = fcntl(pfd[0], F_SETFD, rflags | FD_CLOEXEC);
        assert(frc == 0);

        char round_str[16];
        snprintf(round_str, sizeof(round_str), "%d", i);
        char *argv[] = {
            (char *)PROBE_PATH,
            (char *)"--exit-during-fork-helper",
            (char *)trigger,
            round_str,
            NULL
        };

        pid_t victim = fork();
        if (victim < 0) {
            fprintf(stdout, "exec-handles-probe: exit-during-fork fork failed trigger=%s round=%d errno=%d\n",
                trigger, i, errno);
            (void)close(pfd[0]);
            (void)close(pfd[1]);
            failed++;
            if (failed >= MAX_EXIT_FAILS_PER_TRIGGER) {
                break;
            }
            continue;
        }
        if (victim == 0) {
            (void)close(pfd[0]);
            if (pfd[1] != EXIT_PIPE_FD) {
                if (dup2(pfd[1], EXIT_PIPE_FD) != EXIT_PIPE_FD) {
                    _exit(126);
                }
                (void)close(pfd[1]);
            }
            int wflags = fcntl(EXIT_PIPE_FD, F_GETFD, 0);
            if (wflags >= 0) {
                (void)fcntl(EXIT_PIPE_FD, F_SETFD, wflags & ~FD_CLOEXEC);
            }
            execve(PROBE_PATH, argv, environ);
            _exit(127);
        }

        (void)close(pfd[1]);

        int ready_ok = 0;
        struct pollfd rpf;
        rpf.fd = pfd[0];
        rpf.events = POLLIN;
        rpf.revents = 0;
        for (int tries = 0; tries < 4; tries++) {
            int prc = poll(&rpf, 1, EXIT_READY_TIMEOUT_MS);
            if (prc > 0) {
                char ch = 0;
                if (read(pfd[0], &ch, 1) == 1 && ch == 'R') {
                    ready_ok = 1;
                }
                break;
            }
            if (prc < 0 && errno == EINTR) {
                continue;
            }
            break;
        }

        if (!ready_ok) {
            fprintf(stdout, "exec-handles-probe: exit-during-fork ready-timeout trigger=%s round=%d victim=%ld\n",
                trigger, i, (long)victim);
            (void)kill(victim, SIGKILL);
            int dst = 0;
            (void)wait_child(victim, &dst);
            (void)close(pfd[0]);
            failed++;
            if (failed >= MAX_EXIT_FAILS_PER_TRIGGER) {
                break;
            }
            continue;
        }

        if (is_kill) {
            long jitter_ns = (long)(i % FORK_JITTER_STEPS) * FORK_JITTER_STEP_NS;
            if (jitter_ns > 0) {
                struct timespec req;
                req.tv_sec = 0;
                req.tv_nsec = jitter_ns;
                (void)nanosleep(&req, NULL);
            }
            (void)kill(victim, SIGKILL);
        }

        int vst = 0;
        if (wait_child(victim, &vst) != 0) {
            fprintf(stdout, "exec-handles-probe: exit-during-fork waitpid failed trigger=%s round=%d errno=%d\n",
                trigger, i, errno);
            (void)close(pfd[0]);
            failed++;
            if (failed >= MAX_EXIT_FAILS_PER_TRIGGER) {
                break;
            }
            continue;
        }

        int eof_ok = 0;
        struct pollfd epf;
        epf.fd = pfd[0];
        epf.events = POLLIN;
        epf.revents = 0;
        for (int tries = 0; tries < 4; tries++) {
            int prc = poll(&epf, 1, EXIT_EOF_TIMEOUT_MS);
            if (prc > 0) {
                char b = 0;
                ssize_t n = read(pfd[0], &b, 1);
                if (n == 0) {
                    eof_ok = 1;
                    break;
                }
                if (n < 0 && errno == EINTR) {
                    continue;
                }
                break;
            }
            if (prc < 0 && errno == EINTR) {
                continue;
            }
            break;
        }
        (void)close(pfd[0]);

        if (!eof_ok) {
            eof_timeouts++;
            failed++;
            fprintf(stdout, "exec-handles-probe: eof-timeout trigger=%s round=%d victim_pid=%ld timeout_ms=%d\n",
                trigger, i, (long)victim, EXIT_EOF_TIMEOUT_MS);
            fflush(stdout);
            if (failed >= MAX_EXIT_FAILS_PER_TRIGGER) {
                break;
            }
        }
    }

    fprintf(stdout, "exec-handles-probe: case=exit-during-fork-%s rounds=%d failed=%d eof_timeouts=%d\n",
        trigger, rounds_run, failed, eof_timeouts);
    if (failed == 0) {
        fprintf(stdout, "exec-handles-probe: pass exit-during-fork-%s\n", trigger);
    }
    fflush(stdout);
    return failed;
}

static int run_exit_during_fork_suite(void)
{
    int failed = 0;
    failed += run_exit_during_fork_trigger("exit");
    failed += run_exit_during_fork_trigger("kill");
    hold_for_control();
    if (failed == 0) {
        fprintf(stdout, "exec-handles-probe: PASS\n");
        fflush(stdout);
        return 0;
    }
    fprintf(stdout, "exec-handles-probe: FAIL total_failures=%d\n", failed);
    fflush(stdout);
    return 1;
}

static int exec_timing_helper(void)
{
    int ready_pipe[2];
    int stop_pipe[2];
    if (pipe(ready_pipe) != 0 || pipe(stop_pipe) != 0) {
        fprintf(stderr, "exec-handles-probe: timing pipe failed errno=%d\n", errno);
        return 1;
    }
    int r0 = fcntl(ready_pipe[0], F_GETFD, 0);
    int s1 = fcntl(stop_pipe[1], F_GETFD, 0);
    assert(r0 >= 0 && s1 >= 0);
    assert(fcntl(ready_pipe[0], F_SETFD, r0 | FD_CLOEXEC) == 0);
    assert(fcntl(stop_pipe[1], F_SETFD, s1 | FD_CLOEXEC) == 0);

    uint64_t fork_t0 = now_us();
    for (int i = 0; i < EXEC_TIMING_CHILDREN; i++) {
        pid_t p = fork();
        if (p < 0) {
            fprintf(stderr, "exec-handles-probe: timing fork failed child=%d errno=%d\n", i, errno);
            _exit(2);
        }
        if (p == 0) {
            (void)close(STDOUT_FILENO);
            (void)close(ready_pipe[0]);
            (void)close(stop_pipe[1]);
            char ch = 'R';
            ssize_t w = write(ready_pipe[1], &ch, 1);
            (void)close(ready_pipe[1]);
            if (w == 1) {
                char b = 0;
                (void)read(stop_pipe[0], &b, 1);
            }
            (void)close(stop_pipe[0]);
            _exit(0);
        }
    }
    uint64_t fork_us = now_us() - fork_t0;
    (void)close(ready_pipe[1]);
    (void)close(stop_pipe[0]);

    int got = 0;
    for (int i = 0; i < EXEC_TIMING_CHILDREN * 4 && got < EXEC_TIMING_CHILDREN; i++) {
        char buf[64];
        ssize_t n = read(ready_pipe[0], buf, sizeof(buf));
        if (n > 0) {
            got += (int)n;
        } else if (n < 0 && errno == EINTR) {
            continue;
        } else {
            break;
        }
    }
    (void)close(ready_pipe[0]);
    if (got != EXEC_TIMING_CHILDREN) {
        fprintf(stderr, "exec-handles-probe: timing ready got=%d expected=%d\n", got, EXEC_TIMING_CHILDREN);
        _exit(3);
    }

    uint64_t t0 = now_us();
    char t0_str[32];
    char fork_us_str[32];
    snprintf(t0_str, sizeof(t0_str), "%llu", (unsigned long long)t0);
    snprintf(fork_us_str, sizeof(fork_us_str), "%llu", (unsigned long long)fork_us);
    char *argv[] = {
        (char *)CHECKER_PATH,
        (char *)"--timing-stamp",
        t0_str,
        fork_us_str,
        NULL
    };
    execve(CHECKER_PATH, argv, environ);
    fprintf(stderr, "exec-handles-probe: timing execve failed errno=%d\n", errno);
    _exit(127);
}

static int run_exec_timing_suite(void)
{
    uint64_t samples[EXEC_TIMING_ROUNDS];
    uint64_t fork_samples[EXEC_TIMING_ROUNDS];
    int completed = 0;
    for (int i = 0; i < EXEC_TIMING_ROUNDS; i++) {
        int out_pipe[2];
        if (pipe(out_pipe) != 0) {
            return 1;
        }
        pid_t p = fork();
        if (p < 0) {
            (void)close(out_pipe[0]);
            (void)close(out_pipe[1]);
            return 1;
        }
        if (p == 0) {
            (void)close(out_pipe[0]);
            if (dup2(out_pipe[1], STDOUT_FILENO) != STDOUT_FILENO) {
                _exit(126);
            }
            (void)close(out_pipe[1]);
            char *argv[] = {
                (char *)PROBE_PATH,
                (char *)"--exec-timing-helper",
                NULL
            };
            execve(PROBE_PATH, argv, environ);
            _exit(127);
        }
        (void)close(out_pipe[1]);
        char buf[EXEC_TIMING_BUF_CAP];
        size_t pos = 0;
        for (int k = 0; k < 32 && pos + 1 < sizeof(buf); k++) {
            ssize_t n = read(out_pipe[0], buf + pos, sizeof(buf) - 1 - pos);
            if (n > 0) {
                pos += (size_t)n;
            } else if (n < 0 && errno == EINTR) {
                continue;
            } else {
                break;
            }
        }
        (void)close(out_pipe[0]);
        buf[pos] = '\0';
        int st = 0;
        if (wait_child(p, &st) != 0 || !WIFEXITED(st) || WEXITSTATUS(st) != 0) {
            fprintf(stdout, "exec-handles-probe: timing round=%d failed st=0x%x out=%s\n", i, st, buf);
            return 1;
        }
        char *m = strstr(buf, "timing_us=");
        char *fm = strstr(buf, "fork_100_us=");
        if (!m || !fm) {
            fprintf(stdout, "exec-handles-probe: timing round=%d missing timing fields out=%s\n", i, buf);
            return 1;
        }
        unsigned long long dt = strtoull(m + strlen("timing_us="), NULL, 10);
        unsigned long long fdt = strtoull(fm + strlen("fork_100_us="), NULL, 10);
        samples[completed] = (uint64_t)dt;
        fork_samples[completed] = (uint64_t)fdt;
        completed++;
    }
    assert(completed == EXEC_TIMING_ROUNDS);
    qsort(samples, (size_t)completed, sizeof(uint64_t), cmp_u64);
    qsort(fork_samples, (size_t)completed, sizeof(uint64_t), cmp_u64);
    fprintf(stdout, "exec-handles-probe: case=exec-timing-100-children rounds=%d median_us=%llu min_us=%llu max_us=%llu\n",
        completed,
        (unsigned long long)samples[completed / 2],
        (unsigned long long)samples[0],
        (unsigned long long)samples[completed - 1]);
    fprintf(stdout, "exec-handles-probe: case=fork-100-children rounds=%d median_us=%llu min_us=%llu max_us=%llu\n",
        completed,
        (unsigned long long)fork_samples[completed / 2],
        (unsigned long long)fork_samples[0],
        (unsigned long long)fork_samples[completed - 1]);
    fprintf(stdout, "exec-handles-probe: PASS\n");
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (argc >= 2 && strcmp(argv[1], "--exec-during-fork") == 0) {
        return run_exec_during_fork_suite_mode("exec-during-fork", "--exec-during-fork-helper");
    }
    if (argc >= 3 && strcmp(argv[1], "--exec-during-fork-helper") == 0) {
        return exec_during_fork_helper_n(argv[2], 1);
    }
    if (argc >= 2 && strcmp(argv[1], "--exec-during-fork-two-forkers") == 0) {
        return run_exec_during_fork_suite_mode("exec-during-fork-two-forkers", "--exec-during-fork-two-forkers-helper");
    }
    if (argc >= 3 && strcmp(argv[1], "--exec-during-fork-two-forkers-helper") == 0) {
        return exec_during_fork_helper_n(argv[2], TWO_FORKER_THREADS);
    }
    if (argc >= 2 && strcmp(argv[1], "--exit-during-fork") == 0) {
        return run_exit_during_fork_suite();
    }
    if (argc >= 4 && strcmp(argv[1], "--exit-during-fork-helper") == 0) {
        return exit_during_fork_helper(argv[2], argv[3]);
    }
    if (argc >= 2 && strcmp(argv[1], "--exec-timing") == 0) {
        return run_exec_timing_suite();
    }
    if (argc >= 2 && strcmp(argv[1], "--exec-timing-helper") == 0) {
        return exec_timing_helper();
    }
    if (argc >= 4 && strcmp(argv[1], "--direct-exec-helper") == 0) {
        return direct_exec_helper(argv[2], argv[3]);
    }
    if (argc >= 5 && strcmp(argv[1], "--direct-exec-active-helper") == 0) {
        return direct_exec_active_helper(argv[2], argv[3], argv[4]);
    }

    int total_failures = 0;

    total_failures += run_case("fork-exec-1", "fork-exec", 0, 1, SINGLE_ROUNDS);
    total_failures += run_case("posix-spawn-1", "posix-spawn", 1, 1, SINGLE_ROUNDS);
    total_failures += run_case("direct-exec-multi", "direct-exec", 2, WORKER_THREADS + 1, MULTI_ROUNDS);

    struct worker_state st;
    int rc = pthread_mutex_init(&st.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&st.cv, NULL);
    assert(rc == 0);
    st.ready = 0;
    st.stop = 0;

    pthread_t workers[WORKER_THREADS];
    for (int i = 0; i < WORKER_THREADS; i++) {
        rc = pthread_create(&workers[i], NULL, worker_fn, &st);
        assert(rc == 0);
    }

    pthread_mutex_lock(&st.mu);
    for (int i = 0; i < MAX_COND_WAITS && st.ready < WORKER_THREADS; i++) {
        pthread_cond_wait(&st.cv, &st.mu);
    }
    assert(st.ready == WORKER_THREADS);
    pthread_mutex_unlock(&st.mu);

    total_failures += run_case("fork-exec-multi", "fork-exec", 0, WORKER_THREADS + 1, MULTI_ROUNDS);
    total_failures += run_case("posix-spawn-multi", "posix-spawn", 1, WORKER_THREADS + 1, MULTI_ROUNDS);

    pthread_mutex_lock(&st.mu);
    st.stop = 1;
    pthread_cond_broadcast(&st.cv);
    pthread_mutex_unlock(&st.mu);

    for (int i = 0; i < WORKER_THREADS; i++) {
        rc = pthread_join(workers[i], NULL);
        assert(rc == 0);
    }
    pthread_cond_destroy(&st.cv);
    pthread_mutex_destroy(&st.mu);

    total_failures += run_case("direct-exec-active", "direct-exec-active", 3, WORKER_THREADS + BLOCKED_SIBLINGS + 1, ACTIVE_ROUNDS);

    if (total_failures == 0) {
        fprintf(stdout, "exec-handles-probe: PASS\n");
        return 0;
    }
    fprintf(stdout, "exec-handles-probe: FAIL total_failures=%d\n", total_failures);
    return 1;
}
