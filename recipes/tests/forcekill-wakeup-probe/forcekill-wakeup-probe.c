#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define ROUNDS_PER_CASE 150
#define MAX_FAILS_PER_CASE 3
#define WAIT_THREADS 4
#define READY_TIMEOUT_MS 5000
#define REAP_POLL_ITERS 200
#define REAP_POLL_NS 10000000L
#define WORKER_MAX_ITERS 400000
#define DRIVER_MAX_ITERS 400000
#define MAX_COND_WAITS 4096
#define JITTER_SLOTS 20
#define JITTER_STEP_NS 100000L
#define DRIVER_PACE_NS 50000L
#define MUTEX_HOLD_NS 80000L
#define LONG_SLEEP_SEC 10

enum case_kind {
    CASE_FUTEX = 0,
    CASE_PIPE = 1,
    CASE_NANOSLEEP = 2,
    CASE_UDS = 3,
    CASE_COUNT = 4
};

struct child_sync {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int ready;
};

struct futex_ctx {
    struct child_sync *sync;
    pthread_mutex_t cond_mu;
    pthread_cond_t cond_cv;
    pthread_mutex_t contend_mu;
    uint64_t gen;
    int role;
};

struct fd_ctx {
    struct child_sync *sync;
    int read_fd;
};

static uint64_t now_ms(void)
{
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000ULL + (uint64_t)(ts.tv_nsec / 1000000L);
}

static void noop_handler(int sig)
{
    (void)sig;
}

static void mark_ready(struct child_sync *sync)
{
    pthread_mutex_lock(&sync->mu);
    sync->ready++;
    pthread_cond_broadcast(&sync->cv);
    pthread_mutex_unlock(&sync->mu);
}

static void wait_all_ready(struct child_sync *sync, int expected)
{
    pthread_mutex_lock(&sync->mu);
    for (int i = 0; i < MAX_COND_WAITS && sync->ready < expected; i++) {
        pthread_cond_wait(&sync->cv, &sync->mu);
    }
    assert(sync->ready == expected);
    pthread_mutex_unlock(&sync->mu);
}

static void *futex_cond_worker(void *arg)
{
    struct futex_ctx *ctx = (struct futex_ctx *)arg;
    mark_ready(ctx->sync);
    pthread_mutex_lock(&ctx->cond_mu);
    for (int i = 0; i < WORKER_MAX_ITERS; i++) {
        uint64_t seen = ctx->gen;
        if (ctx->gen == seen) {
            (void)pthread_cond_wait(&ctx->cond_cv, &ctx->cond_mu);
        }
    }
    pthread_mutex_unlock(&ctx->cond_mu);
    return NULL;
}

static void *futex_mutex_worker(void *arg)
{
    struct futex_ctx *ctx = (struct futex_ctx *)arg;
    mark_ready(ctx->sync);
    for (int i = 0; i < WORKER_MAX_ITERS; i++) {
        pthread_mutex_lock(&ctx->contend_mu);
        pthread_mutex_unlock(&ctx->contend_mu);
        (void)sched_yield();
    }
    return NULL;
}

static void *fd_read_worker(void *arg)
{
    struct fd_ctx *ctx = (struct fd_ctx *)arg;
    mark_ready(ctx->sync);
    char b = 0;
    for (int i = 0; i < WORKER_MAX_ITERS; i++) {
        ssize_t n = read(ctx->read_fd, &b, 1);
        if (n < 0 && errno == EINTR) {
            continue;
        }
        if (n <= 0) {
            break;
        }
    }
    return NULL;
}

static void *nanosleep_worker(void *arg)
{
    struct child_sync *sync = (struct child_sync *)arg;
    mark_ready(sync);
    struct timespec req;
    req.tv_sec = LONG_SLEEP_SEC;
    req.tv_nsec = 0;
    for (int i = 0; i < WORKER_MAX_ITERS; i++) {
        (void)nanosleep(&req, NULL);
    }
    return NULL;
}

static void set_nonblock(int fd)
{
    int flags = fcntl(fd, F_GETFL, 0);
    assert(flags >= 0);
    int rc = fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    assert(rc == 0);
}

static void signal_parent_ready(int ready_wfd)
{
    char ch = 'R';
    ssize_t w = write(ready_wfd, &ch, 1);
    assert(w == 1);
    (void)close(ready_wfd);
}

static void run_child_futex(int ready_wfd)
{
    struct child_sync sync;
    int rc = pthread_mutex_init(&sync.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&sync.cv, NULL);
    assert(rc == 0);
    sync.ready = 0;

    struct futex_ctx ctx;
    ctx.sync = &sync;
    ctx.gen = 0;
    ctx.role = 0;
    rc = pthread_mutex_init(&ctx.cond_mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&ctx.cond_cv, NULL);
    assert(rc == 0);
    rc = pthread_mutex_init(&ctx.contend_mu, NULL);
    assert(rc == 0);

    pthread_t threads[WAIT_THREADS];
    for (int i = 0; i < WAIT_THREADS; i++) {
        void *(*fn)(void *) = (i < 2) ? futex_cond_worker : futex_mutex_worker;
        rc = pthread_create(&threads[i], NULL, fn, &ctx);
        assert(rc == 0);
    }
    wait_all_ready(&sync, WAIT_THREADS);
    signal_parent_ready(ready_wfd);

    struct timespec hold_ts;
    hold_ts.tv_sec = 0;
    hold_ts.tv_nsec = MUTEX_HOLD_NS;
    struct timespec pace_ts;
    pace_ts.tv_sec = 0;
    pace_ts.tv_nsec = DRIVER_PACE_NS;

    for (int i = 0; i < DRIVER_MAX_ITERS; i++) {
        pthread_mutex_lock(&ctx.contend_mu);
        (void)nanosleep(&hold_ts, NULL);
        pthread_mutex_unlock(&ctx.contend_mu);

        pthread_mutex_lock(&ctx.cond_mu);
        ctx.gen++;
        pthread_cond_broadcast(&ctx.cond_cv);
        pthread_mutex_unlock(&ctx.cond_mu);
        (void)nanosleep(&pace_ts, NULL);
    }
    _exit(0);
}

static void run_child_fd(int ready_wfd, int is_uds)
{
    int fds[2];
    int rc = is_uds ? socketpair(AF_UNIX, SOCK_STREAM, 0, fds) : pipe(fds);
    assert(rc == 0);
    set_nonblock(fds[1]);

    struct child_sync sync;
    rc = pthread_mutex_init(&sync.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&sync.cv, NULL);
    assert(rc == 0);
    sync.ready = 0;

    struct fd_ctx ctx;
    ctx.sync = &sync;
    ctx.read_fd = fds[0];

    pthread_t threads[WAIT_THREADS];
    for (int i = 0; i < WAIT_THREADS; i++) {
        rc = pthread_create(&threads[i], NULL, fd_read_worker, &ctx);
        assert(rc == 0);
    }
    wait_all_ready(&sync, WAIT_THREADS);
    signal_parent_ready(ready_wfd);

    struct timespec pace_ts;
    pace_ts.tv_sec = 0;
    pace_ts.tv_nsec = DRIVER_PACE_NS;
    const char burst[WAIT_THREADS] = { 'a', 'b', 'c', 'd' };
    for (int i = 0; i < DRIVER_MAX_ITERS; i++) {
        (void)write(fds[1], burst, sizeof(burst));
        (void)nanosleep(&pace_ts, NULL);
    }
    _exit(0);
}

static void run_child_nanosleep(int ready_wfd)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = noop_handler;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    int rc = sigaction(SIGUSR1, &sa, NULL);
    assert(rc == 0);

    struct child_sync sync;
    rc = pthread_mutex_init(&sync.mu, NULL);
    assert(rc == 0);
    rc = pthread_cond_init(&sync.cv, NULL);
    assert(rc == 0);
    sync.ready = 0;

    pthread_t threads[WAIT_THREADS];
    for (int i = 0; i < WAIT_THREADS; i++) {
        rc = pthread_create(&threads[i], NULL, nanosleep_worker, &sync);
        assert(rc == 0);
    }
    wait_all_ready(&sync, WAIT_THREADS);
    signal_parent_ready(ready_wfd);

    struct timespec pace_ts;
    pace_ts.tv_sec = 0;
    pace_ts.tv_nsec = DRIVER_PACE_NS;
    for (int i = 0; i < DRIVER_MAX_ITERS; i++) {
        for (int t = 0; t < WAIT_THREADS; t++) {
            (void)pthread_kill(threads[t], SIGUSR1);
        }
        (void)nanosleep(&pace_ts, NULL);
    }
    _exit(0);
}

static int run_case(const char *name, enum case_kind kind)
{
    int rounds_run = 0;
    int failed = 0;
    int hang_hits = 0;
    uint64_t max_reap_ms = 0;

    struct timespec poll_ts;
    poll_ts.tv_sec = 0;
    poll_ts.tv_nsec = REAP_POLL_NS;

    for (int r = 0; r < ROUNDS_PER_CASE; r++) {
        rounds_run++;
        int rfd[2];
        if (pipe(rfd) != 0) {
            fprintf(stdout, "forcekill-wakeup-probe: error case=%s round=%d pipe errno=%d\n", name, r, errno);
            failed++;
            if (failed >= MAX_FAILS_PER_CASE) {
                break;
            }
            continue;
        }
        pid_t pid = fork();
        if (pid < 0) {
            fprintf(stdout, "forcekill-wakeup-probe: error case=%s round=%d fork errno=%d\n", name, r, errno);
            (void)close(rfd[0]);
            (void)close(rfd[1]);
            failed++;
            if (failed >= MAX_FAILS_PER_CASE) {
                break;
            }
            continue;
        }
        if (pid == 0) {
            (void)close(rfd[0]);
            if (kind == CASE_FUTEX) {
                run_child_futex(rfd[1]);
            } else if (kind == CASE_PIPE) {
                run_child_fd(rfd[1], 0);
            } else if (kind == CASE_NANOSLEEP) {
                run_child_nanosleep(rfd[1]);
            } else {
                run_child_fd(rfd[1], 1);
            }
            _exit(127);
        }

        (void)close(rfd[1]);
        struct pollfd pfd;
        pfd.fd = rfd[0];
        pfd.events = POLLIN;
        pfd.revents = 0;
        int ready_ok = 0;
        for (int tries = 0; tries < 4; tries++) {
            int prc = poll(&pfd, 1, READY_TIMEOUT_MS);
            if (prc > 0) {
                char ch = 0;
                if (read(rfd[0], &ch, 1) == 1 && ch == 'R') {
                    ready_ok = 1;
                }
                break;
            }
            if (prc < 0 && errno == EINTR) {
                continue;
            }
            break;
        }
        (void)close(rfd[0]);

        if (!ready_ok) {
            fprintf(stdout, "forcekill-wakeup-probe: error case=%s round=%d pid=%ld ready_timeout\n",
                name, r, (long)pid);
            (void)kill(pid, SIGKILL);
            failed++;
            if (failed >= MAX_FAILS_PER_CASE) {
                break;
            }
            continue;
        }

        uint32_t mix = (uint32_t)(r + 1) * 2654435761U;
        long jitter_ns = (long)((mix >> 16) % JITTER_SLOTS) * JITTER_STEP_NS;
        if (jitter_ns > 0) {
            struct timespec jts;
            jts.tv_sec = 0;
            jts.tv_nsec = jitter_ns;
            (void)nanosleep(&jts, NULL);
        }

        uint64_t t0 = now_ms();
        int krc = kill(pid, SIGKILL);
        if (krc != 0) {
            fprintf(stdout, "forcekill-wakeup-probe: error case=%s round=%d pid=%ld kill errno=%d\n",
                name, r, (long)pid, errno);
            failed++;
            if (failed >= MAX_FAILS_PER_CASE) {
                break;
            }
            continue;
        }

        int reaped = 0;
        int st = 0;
        for (int p = 0; p < REAP_POLL_ITERS; p++) {
            pid_t w = waitpid(pid, &st, WNOHANG);
            if (w == pid) {
                reaped = 1;
                break;
            }
            if (w < 0 && errno != EINTR) {
                break;
            }
            (void)nanosleep(&poll_ts, NULL);
        }
        uint64_t dt_ms = now_ms() - t0;
        if (reaped) {
            if (dt_ms > max_reap_ms) {
                max_reap_ms = dt_ms;
            }
            if (!WIFSIGNALED(st) || WTERMSIG(st) != SIGKILL) {
                fprintf(stdout, "forcekill-wakeup-probe: error case=%s round=%d pid=%ld status=0x%x\n",
                    name, r, (long)pid, st);
                failed++;
                if (failed >= MAX_FAILS_PER_CASE) {
                    break;
                }
            }
        } else {
            hang_hits++;
            failed++;
            fprintf(stdout, "forcekill-wakeup-probe: hang case=%s round=%d pid=%ld waited_ms=%llu\n",
                name, r, (long)pid, (unsigned long long)dt_ms);
            fflush(stdout);
            if (hang_hits >= MAX_FAILS_PER_CASE) {
                break;
            }
        }
    }

    fprintf(stdout, "forcekill-wakeup-probe: case=%s rounds=%d failed=%d hang_hits=%d max_reap_ms=%llu\n",
        name, rounds_run, failed, hang_hits, (unsigned long long)max_reap_ms);
    if (failed == 0) {
        fprintf(stdout, "forcekill-wakeup-probe: pass %s\n", name);
    }
    fflush(stdout);
    return failed;
}

int main(void)
{
    setvbuf(stdout, NULL, _IOLBF, 0);
    const char *names[CASE_COUNT] = { "futex", "pipe", "nanosleep", "uds" };
    int total_failures = 0;
    for (int i = 0; i < CASE_COUNT; i++) {
        total_failures += run_case(names[i], (enum case_kind)i);
    }
    if (total_failures == 0) {
        fprintf(stdout, "forcekill-wakeup-probe: PASS\n");
        fflush(stdout);
        return 0;
    }
    fprintf(stdout, "forcekill-wakeup-probe: FAIL total_failures=%d\n", total_failures);
    fflush(stdout);
    return 1;
}
