#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define PROBE_PATH "/usr/bin/exec-handles-probe"
#define CHECKER_PATH "/usr/bin/exec-handles-checker"
#define WORKER_THREADS 8
#define SINGLE_ROUNDS 3
#define MULTI_ROUNDS 20
#define MAX_ROUNDS 64
#define MAX_WAIT_ITERS 64
#define MAX_COND_WAITS 1024
#define MAX_DIRECT_SLEEPS 2000
#define DIRECT_SLEEP_NS 5000000L

extern char **environ;

struct worker_state {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int ready;
    int stop;
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

static int run_case(const char *case_name, const char *mode, int mode_kind, int nthreads, int rounds)
{
    assert(rounds > 0 && rounds <= MAX_ROUNDS);
    char threads_str[16];
    snprintf(threads_str, sizeof(threads_str), "%d", nthreads);

    uint64_t times[MAX_ROUNDS];
    int completed = 0;
    int failed = 0;
    for (int i = 0; i < rounds; i++) {
        const char *verbose_str = (i == 0) ? "1" : "0";
        uint64_t dt = 0;
        int rc;
        if (mode_kind == 0) {
            rc = run_fork_exec(mode, threads_str, verbose_str, &dt);
        } else if (mode_kind == 1) {
            rc = run_posix_spawn(mode, threads_str, verbose_str, &dt);
        } else {
            rc = run_direct_exec(threads_str, verbose_str, &dt);
        }
        if (rc != 0) {
            failed++;
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

    fprintf(stdout, "exec-handles-probe: case=%s threads=%d rounds=%d failed=%d median_us=%llu\n",
        mode, nthreads, rounds, failed, (unsigned long long)median_us);
    if (failed == 0) {
        fprintf(stdout, "exec-handles-probe: pass %s\n", case_name);
    }
    fflush(stdout);
    return failed;
}

int main(int argc, char **argv)
{
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (argc >= 4 && strcmp(argv[1], "--direct-exec-helper") == 0) {
        return direct_exec_helper(argv[2], argv[3]);
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

    if (total_failures == 0) {
        fprintf(stdout, "exec-handles-probe: PASS\n");
        return 0;
    }
    fprintf(stdout, "exec-handles-probe: FAIL total_failures=%d\n", total_failures);
    return 1;
}
