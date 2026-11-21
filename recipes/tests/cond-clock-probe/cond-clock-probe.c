#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <time.h>

static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t condition;

static void *signal_after_delay(void *argument)
{
    (void)argument;
    const struct timespec delay = { .tv_sec = 0, .tv_nsec = 100000000 };
    nanosleep(&delay, NULL);
    pthread_mutex_lock(&mutex);
    pthread_cond_signal(&condition);
    pthread_mutex_unlock(&mutex);
    return NULL;
}

static int check_clock(clockid_t clock, const char *name)
{
    pthread_condattr_t attr;
    int error = pthread_condattr_init(&attr);
    if (!error)
        error = pthread_condattr_setclock(&attr, clock);
    if (!error)
        error = pthread_cond_init(&condition, &attr);
    pthread_condattr_destroy(&attr);
    if (error) {
        printf("cond-clock-probe: FAIL %s-init (%d)\n", name, error);
        return 1;
    }

    pthread_mutex_lock(&mutex);
    pthread_t thread;
    error = pthread_create(&thread, NULL, signal_after_delay, NULL);
    if (error) {
        printf("cond-clock-probe: FAIL %s-pthread-create (%d)\n", name, error);
        pthread_mutex_unlock(&mutex);
        return 1;
    }

    struct timespec deadline;
    clock_gettime(clock, &deadline);
    deadline.tv_sec += 5;
    error = pthread_cond_timedwait(&condition, &mutex, &deadline);
    pthread_mutex_unlock(&mutex);
    pthread_join(thread, NULL);
    pthread_cond_destroy(&condition);
    if (error) {
        printf("cond-clock-probe: FAIL %s-signal (error %d, expected 0, ETIMEDOUT %d)\n",
               name, error, ETIMEDOUT);
        return 1;
    }
    printf("cond-clock-probe: pass %s-signal\n", name);
    return 0;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    if (check_clock(CLOCK_REALTIME, "realtime"))
        return 1;
    if (check_clock(CLOCK_MONOTONIC, "monotonic"))
        return 1;
    puts("cond-clock-probe: PASS");
    return 0;
}
