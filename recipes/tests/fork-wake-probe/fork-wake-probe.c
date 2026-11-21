#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static pthread_mutex_t held = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t cond_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond;
static int ready;
static atomic_int mutex_waiter_done;
static atomic_int cond_waiter_done;
static atomic_int waiters_started;

static void fail(const char *what) {
    fprintf(stderr, "fork-wake-probe: FAIL %s\n", what);
    exit(1);
}

static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
    }
}

static void *mutex_waiter(void *arg) {
    (void)arg;
    atomic_fetch_add(&waiters_started, 1);
    pthread_mutex_lock(&held);
    pthread_mutex_unlock(&held);
    atomic_store(&mutex_waiter_done, 1);
    return NULL;
}

static void *cond_waiter(void *arg) {
    (void)arg;
    pthread_mutex_lock(&cond_lock);
    atomic_fetch_add(&waiters_started, 1);
    while (!ready) {
        pthread_cond_wait(&cond, &cond_lock);
    }
    pthread_mutex_unlock(&cond_lock);
    atomic_store(&cond_waiter_done, 1);
    return NULL;
}

static int wait_flag(atomic_int *flag) {
    for (int i = 0; i < 300; i++) {
        if (atomic_load(flag)) {
            return 1;
        }
        sleep_ms(10);
    }
    return 0;
}

int main(void) {
    alarm(30);
    pthread_condattr_t attr;
    if (pthread_condattr_init(&attr) != 0 || pthread_condattr_setclock(&attr, CLOCK_MONOTONIC) != 0 ||
        pthread_cond_init(&cond, &attr) != 0) {
        fail("condition variable setup");
    }

    pthread_mutex_lock(&held);
    pthread_t threads[2];
    if (pthread_create(&threads[0], NULL, mutex_waiter, NULL) != 0 ||
        pthread_create(&threads[1], NULL, cond_waiter, NULL) != 0) {
        fail("pthread_create");
    }
    while (atomic_load(&waiters_started) < 2) {
        sleep_ms(10);
    }
    sleep_ms(300);

    pid_t child = fork();
    if (child < 0) {
        fail("fork");
    }
    if (child == 0) {
        sleep_ms(4000);
        _exit(0);
    }

    int failed = 0;
    pthread_mutex_unlock(&held);
    if (wait_flag(&mutex_waiter_done)) {
        printf("fork-wake-probe: pass mutex\n");
    } else {
        printf("fork-wake-probe: FAIL mutex waiter blocked before fork was not woken by unlock\n");
        failed = 1;
    }

    pthread_mutex_lock(&cond_lock);
    ready = 1;
    pthread_cond_signal(&cond);
    pthread_mutex_unlock(&cond_lock);
    if (wait_flag(&cond_waiter_done)) {
        printf("fork-wake-probe: pass cond\n");
    } else {
        printf("fork-wake-probe: FAIL condition waiter blocked before fork was not woken by signal\n");
        failed = 1;
    }
    if (failed) {
        return 1;
    }

    pthread_join(threads[0], NULL);
    pthread_join(threads[1], NULL);
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fail("child exit");
    }
    printf("fork-wake-probe: PASS\n");
    return 0;
}
