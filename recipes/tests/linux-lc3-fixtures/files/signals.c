#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#define SIGSUSPEND_ROUNDS 2000
#define SIGSUSPEND_IGN_ROUNDS 20
#define PAUSE_ROUNDS 500
#define HELD_SIGNAL_MS 100
#define RETRY_LIMIT 500
#define WAIT_LIMIT_MS 2000
#define PAUSE_WATCHDOG_MS 2000
#define FAST_SPIN_ITERS 16384
#define MAX_TIMEOUTS 5
#define NUDGE_LIMIT 20

static atomic_int usr1_count;
static atomic_llong last_usr1_ms;
static atomic_llong last_alrm_ms;
static atomic_int usr2_count;
static atomic_int alrm_count;
static atomic_int stealer_hits;
static atomic_int waiter_waiting;
static atomic_int waiter_abort;
static atomic_int stealer_stop;
static atomic_int pause_seq;
static atomic_int pause_done;
static atomic_int pause_abort;
static pthread_t waiter_tid;
static pthread_t stealer_tid;

static int64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void on_usr1(int sig) {
    (void)sig;
    atomic_store(&last_usr1_ms, now_ms());
    atomic_fetch_add(&usr1_count, 1);
    if (pthread_equal(pthread_self(), stealer_tid)) {
        atomic_fetch_add(&stealer_hits, 1);
    }
}

static void on_usr2(int sig) {
    (void)sig;
    atomic_fetch_add(&usr2_count, 1);
}

static void on_alrm(int sig) {
    (void)sig;
    atomic_store(&last_alrm_ms, now_ms());
    atomic_fetch_add(&alrm_count, 1);
}

static int wait_eq_ms(atomic_int *v, int want, atomic_int *abort_flag, int64_t limit_ms) {
    int64_t deadline = now_ms() + limit_ms;
    for (int i = 0; i < FAST_SPIN_ITERS; i++) {
        if (atomic_load(abort_flag)) {
            return 0;
        }
        if (atomic_load(v) == want) {
            return 1;
        }
        if ((i & 255) == 255) {
            if (now_ms() >= deadline) {
                return 0;
            }
            sched_yield();
        }
    }
    for (int64_t step = 0; step < limit_ms; step++) {
        if (atomic_load(abort_flag)) {
            return 0;
        }
        if (atomic_load(v) == want) {
            return 1;
        }
        if (now_ms() >= deadline) {
            return 0;
        }
        usleep(1000);
    }
    return atomic_load(v) == want;
}

static int wait_ne_ms(atomic_int *v, int cur, atomic_int *abort_flag, int64_t limit_ms) {
    int64_t deadline = now_ms() + limit_ms;
    for (int i = 0; i < FAST_SPIN_ITERS; i++) {
        if (atomic_load(abort_flag)) {
            return 0;
        }
        if (atomic_load(v) != cur) {
            return 1;
        }
        if ((i & 255) == 255) {
            if (now_ms() >= deadline) {
                return 0;
            }
            sched_yield();
        }
    }
    for (int64_t step = 0; step < limit_ms; step++) {
        if (atomic_load(abort_flag)) {
            return 0;
        }
        if (atomic_load(v) != cur) {
            return 1;
        }
        if (now_ms() >= deadline) {
            return 0;
        }
        usleep(1000);
    }
    return atomic_load(v) != cur;
}

static int wait_ge_ms(atomic_int *v, int want, atomic_int *abort_flag, int64_t limit_ms) {
    int64_t deadline = now_ms() + limit_ms;
    for (int i = 0; i < FAST_SPIN_ITERS; i++) {
        if (atomic_load(abort_flag)) {
            return 0;
        }
        if (atomic_load(v) >= want) {
            return 1;
        }
        if ((i & 255) == 255) {
            if (now_ms() >= deadline) {
                return 0;
            }
            sched_yield();
        }
    }
    for (int64_t step = 0; step < limit_ms; step++) {
        if (atomic_load(abort_flag)) {
            return 0;
        }
        if (atomic_load(v) >= want) {
            return 1;
        }
        if (now_ms() >= deadline) {
            return 0;
        }
        usleep(1000);
    }
    return atomic_load(v) >= want;
}

static void *stealer_fn(void *arg) {
    (void)arg;
    sigset_t set;
    sigemptyset(&set);
    sigaddset(&set, SIGUSR2);
    sigaddset(&set, SIGALRM);
    pthread_sigmask(SIG_SETMASK, &set, NULL);
    while (!atomic_load(&stealer_stop)) {
        (void)getpid();
    }
    return NULL;
}

static void *sigsuspend_sender_fn(void *arg) {
    (void)arg;
    sigset_t all;
    sigemptyset(&all);
    sigaddset(&all, SIGUSR1);
    sigaddset(&all, SIGUSR2);
    sigaddset(&all, SIGALRM);
    pthread_sigmask(SIG_SETMASK, &all, NULL);
    pid_t me = getpid();
    for (int r = 1; r <= SIGSUSPEND_ROUNDS && !atomic_load(&waiter_abort); r++) {
        if (!wait_eq_ms(&waiter_waiting, r, &waiter_abort, WAIT_LIMIT_MS)) {
            continue;
        }
        int before = atomic_load(&stealer_hits);
        kill(me, SIGUSR1);
        int64_t deadline = now_ms() + WAIT_LIMIT_MS;
        int woke = 0;
        for (int i = 0; i < FAST_SPIN_ITERS && !atomic_load(&waiter_abort); i++) {
            if (atomic_load(&waiter_waiting) != r) {
                woke = 1;
                break;
            }
            if (atomic_load(&stealer_hits) != before) {
                before = atomic_load(&stealer_hits);
                pthread_kill(waiter_tid, SIGUSR1);
            }
            if ((i & 255) == 255) {
                if (now_ms() >= deadline) {
                    break;
                }
                sched_yield();
            }
        }
        for (int64_t step = 0; !woke && step < WAIT_LIMIT_MS && !atomic_load(&waiter_abort); step++) {
            if (atomic_load(&waiter_waiting) != r) {
                woke = 1;
                break;
            }
            if (atomic_load(&stealer_hits) != before) {
                before = atomic_load(&stealer_hits);
                pthread_kill(waiter_tid, SIGUSR1);
            }
            if (now_ms() >= deadline) {
                break;
            }
            usleep(1000);
        }
        if (!woke) {
            for (int n = 0; n < NUDGE_LIMIT && atomic_load(&waiter_waiting) == r; n++) {
                pthread_kill(waiter_tid, SIGALRM);
                usleep(10000);
            }
        }
    }
    return NULL;
}

static void *ign_sender_fn(void *arg) {
    (void)arg;
    sigset_t all;
    sigemptyset(&all);
    sigaddset(&all, SIGUSR1);
    sigaddset(&all, SIGUSR2);
    sigaddset(&all, SIGALRM);
    pthread_sigmask(SIG_SETMASK, &all, NULL);
    for (int r = 1; r <= SIGSUSPEND_IGN_ROUNDS && !atomic_load(&waiter_abort); r++) {
        if (!wait_eq_ms(&waiter_waiting, r, &waiter_abort, WAIT_LIMIT_MS)) {
            continue;
        }
        usleep(2000);
        pthread_kill(waiter_tid, SIGUSR2);
        usleep(5000);
        if (atomic_load(&waiter_waiting) == r) {
            pthread_kill(waiter_tid, SIGUSR1);
        }
        if (!wait_ne_ms(&waiter_waiting, r, &waiter_abort, WAIT_LIMIT_MS)) {
            for (int n = 0; n < NUDGE_LIMIT && atomic_load(&waiter_waiting) == r; n++) {
                pthread_kill(waiter_tid, SIGALRM);
                usleep(10000);
            }
        }
    }
    return NULL;
}

static void *ppoll_sender_fn(void *arg) {
    (void)arg;
    sigset_t all;
    sigemptyset(&all);
    sigaddset(&all, SIGUSR1);
    sigaddset(&all, SIGUSR2);
    sigaddset(&all, SIGALRM);
    pthread_sigmask(SIG_SETMASK, &all, NULL);
    (void)wait_eq_ms(&waiter_waiting, 1, &waiter_abort, WAIT_LIMIT_MS);
    usleep(5000);
    kill(getpid(), SIGUSR1);
    return NULL;
}

static void *pause_sender_fn(void *arg) {
    (void)arg;
    sigset_t all;
    sigemptyset(&all);
    sigaddset(&all, SIGUSR1);
    sigaddset(&all, SIGUSR2);
    sigaddset(&all, SIGALRM);
    pthread_sigmask(SIG_SETMASK, &all, NULL);
    pid_t me = getpid();
    int max_seq = PAUSE_ROUNDS + RETRY_LIMIT;
    for (int seq = 1; seq <= max_seq && !atomic_load(&pause_abort); seq++) {
        if (!wait_eq_ms(&pause_seq, seq, &pause_abort, PAUSE_WATCHDOG_MS)) {
            continue;
        }
        usleep(5000);
        kill(me, SIGUSR1);
        if (!wait_ge_ms(&pause_done, seq, &pause_abort, PAUSE_WATCHDOG_MS)) {
            for (int n = 0; n < NUDGE_LIMIT && atomic_load(&pause_done) < seq && !atomic_load(&pause_abort); n++) {
                pthread_kill(waiter_tid, SIGALRM);
                usleep(10000);
            }
        }
    }
    return NULL;
}

static int mask_has_both(const sigset_t *s) {
    return sigismember(s, SIGUSR1) == 1 && sigismember(s, SIGUSR2) == 1;
}

int main(void) {
    waiter_tid = pthread_self();

    struct sigaction sa = {0};
    sa.sa_handler = on_usr1;
    sigemptyset(&sa.sa_mask);
    if (sigaction(SIGUSR1, &sa, NULL) != 0) {
        return 2;
    }
    sa.sa_handler = on_alrm;
    if (sigaction(SIGALRM, &sa, NULL) != 0) {
        return 2;
    }

    sigset_t saved, empty, cur;
    sigemptyset(&saved);
    sigaddset(&saved, SIGUSR1);
    sigaddset(&saved, SIGUSR2);
    sigemptyset(&empty);
    pthread_sigmask(SIG_SETMASK, &saved, NULL);

    int kept = 0;
    int timed_out = 0;
    atomic_store(&stealer_stop, 0);
    atomic_store(&waiter_waiting, 0);
    atomic_store(&waiter_abort, 0);
    if (pthread_create(&stealer_tid, NULL, stealer_fn, NULL) != 0) {
        return 3;
    }
    pthread_t sender;
    if (pthread_create(&sender, NULL, sigsuspend_sender_fn, NULL) != 0) {
        return 3;
    }
    for (int r = 1; r <= SIGSUSPEND_ROUNDS; r++) {
        int alrm_before = atomic_load(&alrm_count);
        atomic_store(&waiter_waiting, r);
        sigsuspend(&empty);
        pthread_sigmask(SIG_SETMASK, NULL, &cur);
        if (mask_has_both(&cur)) {
            kept++;
        } else {
            pthread_sigmask(SIG_SETMASK, &saved, NULL);
        }
        atomic_store(&waiter_waiting, 0);
        if (atomic_load(&alrm_count) != alrm_before) {
            timed_out++;
            if (timed_out >= MAX_TIMEOUTS) {
                atomic_store(&waiter_abort, 1);
                break;
            }
        }
    }
    atomic_store(&waiter_abort, 1);
    pthread_join(sender, NULL);
    atomic_store(&stealer_stop, 1);
    pthread_join(stealer_tid, NULL);
    printf("sigsuspend mask kept %d/%d\n", kept, SIGSUSPEND_ROUNDS);
    if (timed_out > 0) {
        printf("sigsuspend timed out %d\n", timed_out);
    }

    sa.sa_handler = SIG_IGN;
    sigaction(SIGUSR2, &sa, NULL);
    int ign_kept = 0;
    atomic_store(&waiter_waiting, 0);
    atomic_store(&waiter_abort, 0);
    if (pthread_create(&sender, NULL, ign_sender_fn, NULL) != 0) {
        return 4;
    }
    for (int r = 1; r <= SIGSUSPEND_IGN_ROUNDS; r++) {
        atomic_store(&waiter_waiting, r);
        sigsuspend(&empty);
        pthread_sigmask(SIG_SETMASK, NULL, &cur);
        if (mask_has_both(&cur)) {
            ign_kept++;
        } else {
            pthread_sigmask(SIG_SETMASK, &saved, NULL);
        }
        atomic_store(&waiter_waiting, 0);
        sigset_t pend;
        sigpending(&pend);
        if (sigismember(&pend, SIGUSR1) == 1) {
            sigsuspend(&empty);
            pthread_sigmask(SIG_SETMASK, &saved, NULL);
        }
    }
    atomic_store(&waiter_abort, 1);
    pthread_join(sender, NULL);
    printf("sigsuspend ign mask kept %d/%d\n", ign_kept, SIGSUSPEND_IGN_ROUNDS);

    int pfd_pair[2];
    if (pipe(pfd_pair) != 0) {
        return 5;
    }
    struct pollfd pfd = {.fd = pfd_pair[0], .events = POLLIN, .revents = 0};
    struct timespec ts = {.tv_sec = 2, .tv_nsec = 0};
    int usr1_before = atomic_load(&usr1_count);
    atomic_store(&waiter_waiting, 0);
    atomic_store(&waiter_abort, 0);
    if (pthread_create(&sender, NULL, ppoll_sender_fn, NULL) != 0) {
        return 5;
    }
    atomic_store(&waiter_waiting, 1);
    int pr = ppoll(&pfd, 1, &ts, &empty);
    int perr = errno;
    pthread_join(sender, NULL);
    pthread_sigmask(SIG_SETMASK, NULL, &cur);
    int ppoll_ok = (pr == -1 && perr == EINTR && atomic_load(&usr1_count) > usr1_before && mask_has_both(&cur));
    printf("ppoll mask kept %d/1\n", ppoll_ok);
    close(pfd_pair[0]);
    close(pfd_pair[1]);

    sa.sa_handler = on_usr2;
    sigaction(SIGUSR2, &sa, NULL);
    sigset_t pause_mask;
    sigemptyset(&pause_mask);
    sigaddset(&pause_mask, SIGUSR2);
    pthread_sigmask(SIG_SETMASK, &pause_mask, NULL);

    int pause_ok = 0;
    int pause_missed = 0;
    int retries = 0;
    int seq = 0;
    atomic_store(&pause_seq, 0);
    atomic_store(&pause_done, 0);
    atomic_store(&pause_abort, 0);
    if (pthread_create(&sender, NULL, pause_sender_fn, NULL) != 0) {
        return 6;
    }
    while (pause_ok < PAUSE_ROUNDS && retries < RETRY_LIMIT) {
        seq++;
        int alrm_before = atomic_load(&alrm_count);
        int usr1_before_p = atomic_load(&usr1_count);
        int entered_pause = 0;
        atomic_store(&pause_seq, seq);
        if (atomic_load(&usr1_count) == usr1_before_p) {
            entered_pause = 1;
            pause();
        }
        atomic_store(&pause_done, seq);
        if (!entered_pause) {
            retries++;
            continue;
        }
        if (atomic_load(&alrm_count) != alrm_before) {
            int usr1_ran = atomic_load(&usr1_count) != usr1_before_p;
            int64_t gap = atomic_load(&last_alrm_ms) - atomic_load(&last_usr1_ms);
            if (!usr1_ran || (gap >= -HELD_SIGNAL_MS && gap <= HELD_SIGNAL_MS)) {
                pause_missed++;
                if (pause_missed >= MAX_TIMEOUTS) {
                    atomic_store(&pause_abort, 1);
                    break;
                }
            } else {
                retries++;
            }
        } else {
            pause_ok++;
        }
    }
    atomic_store(&pause_abort, 1);
    pthread_join(sender, NULL);
    printf("pause ok %d/%d\n", pause_ok, PAUSE_ROUNDS);
    if (pause_missed > 0) {
        printf("pause missed %d\n", pause_missed);
    }
    return 0;
}
