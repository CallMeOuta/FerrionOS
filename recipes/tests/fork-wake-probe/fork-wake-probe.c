#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern size_t redox_futex_wait_v0(uint32_t *addr, uint32_t val, const struct timespec *deadline);
extern size_t redox_futex_wake_v0(uint32_t *addr, uint32_t num);

static pthread_mutex_t held = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t cond_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t cond;
static int ready;
static atomic_int mutex_waiter_done;
static atomic_int cond_waiter_done;
static atomic_int waiters_started;

static uint32_t priv_reuse_word;

static void fail(const char *what) {
    fprintf(stderr, "fork-wake-probe: FAIL %s\n", what);
    exit(1);
}

static void sleep_ms(long ms) {
    struct timespec ts = {ms / 1000, (ms % 1000) * 1000000L};
    while (nanosleep(&ts, &ts) != 0 && errno == EINTR) {
    }
}

static struct timespec mono_after_ms(long ms) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        fail("clock_gettime");
    }
    ts.tv_sec += ms / 1000;
    ts.tv_nsec += (ms % 1000) * 1000000L;
    if (ts.tv_nsec >= 1000000000L) {
        ts.tv_sec += 1;
        ts.tv_nsec -= 1000000000L;
    }
    return ts;
}

static ssize_t futex_wait_raw(uint32_t *word, uint32_t expected, const struct timespec *deadline) {
    return (ssize_t)redox_futex_wait_v0(word, expected, deadline);
}

static ssize_t futex_wake_raw(uint32_t *word, uint32_t count) {
    return (ssize_t)redox_futex_wake_v0(word, count);
}

static long read_futex_subs(void) {
    int fd = open("/scheme/sys/stat", O_RDONLY);
    if (fd < 0) {
        return -1;
    }
    char buf[4096];
    ssize_t n = read(fd, buf, sizeof(buf) - 1);
    close(fd);
    if (n <= 0) {
        return -1;
    }
    buf[n] = '\0';
    const char *p = strstr(buf, "futex_subcribers:");
    if (!p) {
        p = strstr(buf, "futex_subscribers:");
    }
    if (!p) {
        return -1;
    }
    p = strchr(p, ':');
    if (!p) {
        return -1;
    }
    return strtol(p + 1, NULL, 10);
}

static int wait_flag_ms(atomic_int *flag, int limit_ms) {
    int steps = limit_ms / 10;
    for (int i = 0; i < steps; i++) {
        if (atomic_load(flag)) {
            return 1;
        }
        sleep_ms(10);
    }
    return atomic_load(flag) != 0;
}

static int wait_futex_subs_le(long limit, int max_ms) {
    int steps = max_ms / 20;
    for (int i = 0; i < steps; i++) {
        long cur = read_futex_subs();
        if (cur >= 0 && cur <= limit) {
            return 1;
        }
        sleep_ms(20);
    }
    long cur = read_futex_subs();
    return cur >= 0 && cur <= limit;
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

static int case_fork_basic(void) {
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
    if (wait_flag_ms(&mutex_waiter_done, 3000)) {
        printf("fork-wake-probe: pass mutex\n");
    } else {
        printf("fork-wake-probe: FAIL mutex waiter blocked before fork was not woken by unlock\n");
        failed = 1;
    }

    pthread_mutex_lock(&cond_lock);
    ready = 1;
    pthread_cond_signal(&cond);
    pthread_mutex_unlock(&cond_lock);
    if (wait_flag_ms(&cond_waiter_done, 3000)) {
        printf("fork-wake-probe: pass cond\n");
    } else {
        printf("fork-wake-probe: FAIL condition waiter blocked before fork was not woken by signal\n");
        failed = 1;
    }

    if (!failed) {
        pthread_join(threads[0], NULL);
        pthread_join(threads[1], NULL);
    }
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fail("child exit");
    }
    return failed;
}

static volatile sig_atomic_t usr1_hits;

static void on_usr1(int sig) {
    (void)sig;
    usr1_hits++;
}

struct sig_race_ctx {
    uint32_t *word;
    atomic_int entered;
    atomic_int done;
    atomic_long rc;
};

static void *sig_race_waiter(void *arg) {
    struct sig_race_ctx *c = (struct sig_race_ctx *)arg;
    atomic_store(&c->entered, 1);
    ssize_t r = futex_wait_raw(c->word, 0, NULL);
    atomic_store(&c->rc, (long)r);
    atomic_store(&c->done, 1);
    return NULL;
}

static int case_wake_signal_race(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_usr1;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = 0;
    if (sigaction(SIGUSR1, &sa, NULL) != 0) {
        printf("fork-wake-probe: FAIL wake-signal-race sigaction failed\n");
        return 1;
    }

    int lost_wakes = 0;
    uint32_t word = 0;
    for (int round = 0; round < 64; round++) {
        word = 0;
        struct sig_race_ctx c1 = {.word = &word};
        struct sig_race_ctx c2 = {.word = &word};
        pthread_t t1, t2;
        if (pthread_create(&t1, NULL, sig_race_waiter, &c1) != 0 ||
            pthread_create(&t2, NULL, sig_race_waiter, &c2) != 0) {
            printf("fork-wake-probe: FAIL wake-signal-race pthread_create\n");
            return 1;
        }
        wait_flag_ms(&c1.entered, 1000);
        wait_flag_ms(&c2.entered, 1000);
        sleep_ms(15);

        ssize_t woken = futex_wake_raw(&word, 1);
        pthread_kill(t1, SIGUSR1);
        pthread_kill(t2, SIGUSR1);
        sleep_ms(20);

        int got_zero = 0;
        if (atomic_load(&c1.done) && atomic_load(&c1.rc) == 0) {
            got_zero++;
        }
        if (atomic_load(&c2.done) && atomic_load(&c2.rc) == 0) {
            got_zero++;
        }
        if (woken == 1 && got_zero == 0) {
            lost_wakes++;
        }

        word = 1;
        futex_wake_raw(&word, 16);
        pthread_join(t1, NULL);
        pthread_join(t2, NULL);
    }

    if (lost_wakes == 0) {
        printf("fork-wake-probe: pass wake-signal-race\n");
        return 0;
    }
    printf("fork-wake-probe: FAIL wake-signal-race lost %d/64 consumed wakes to EINTR\n", lost_wakes);
    return 1;
}

struct simple_waiter_ctx {
    uint32_t *word;
    const struct timespec *deadline;
    atomic_int entered;
    atomic_int done;
    atomic_long rc;
};

static void *simple_futex_waiter(void *arg) {
    struct simple_waiter_ctx *c = (struct simple_waiter_ctx *)arg;
    atomic_store(&c->entered, 1);
    ssize_t r = futex_wait_raw(c->word, 0, c->deadline);
    atomic_store(&c->rc, (long)r);
    atomic_store(&c->done, 1);
    return NULL;
}

static int case_killed_shared_wake(void) {
    long base_subs = read_futex_subs();
    uint32_t *shared = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (shared == MAP_FAILED) {
        printf("fork-wake-probe: FAIL killed-shared-wake mmap failed\n");
        return 1;
    }

    int lost_wakes = 0;
    for (int round = 0; round < 24; round++) {
        *shared = 0;
        int pfd[2];
        if (pipe(pfd) != 0) {
            munmap(shared, 4096);
            printf("fork-wake-probe: FAIL killed-shared-wake pipe failed\n");
            return 1;
        }
        fflush(stdout);
        pid_t victim = fork();
        if (victim < 0) {
            close(pfd[0]);
            close(pfd[1]);
            munmap(shared, 4096);
            printf("fork-wake-probe: FAIL killed-shared-wake fork failed\n");
            return 1;
        }
        if (victim == 0) {
            close(pfd[0]);
            char b = 1;
            if (write(pfd[1], &b, 1) != 1) {
                _exit(2);
            }
            close(pfd[1]);
            futex_wait_raw(shared, 0, NULL);
            _exit(0);
        }
        close(pfd[1]);
        char b = 0;
        if (read(pfd[0], &b, 1) != 1) {
            kill(victim, SIGKILL);
            waitpid(victim, NULL, 0);
            close(pfd[0]);
            munmap(shared, 4096);
            printf("fork-wake-probe: FAIL killed-shared-wake child handshake\n");
            return 1;
        }
        close(pfd[0]);
        sleep_ms(30);

        struct simple_waiter_ctx wb = {.word = shared, .deadline = NULL};
        pthread_t tb;
        if (pthread_create(&tb, NULL, simple_futex_waiter, &wb) != 0) {
            kill(victim, SIGKILL);
            waitpid(victim, NULL, 0);
            munmap(shared, 4096);
            printf("fork-wake-probe: FAIL killed-shared-wake pthread_create\n");
            return 1;
        }
        wait_flag_ms(&wb.entered, 1000);
        sleep_ms(30);

        *shared = 1;
        kill(victim, SIGKILL);
        ssize_t woken = futex_wake_raw(shared, 1);
        int b_woke = wait_flag_ms(&wb.done, 400);
        if (woken == 1 && !b_woke) {
            lost_wakes++;
        }
        if (!b_woke) {
            futex_wake_raw(shared, 16);
            wait_flag_ms(&wb.done, 2000);
        }
        pthread_join(tb, NULL);
        waitpid(victim, NULL, 0);
    }

    munmap(shared, 4096);

    int subs_ok = (base_subs >= 0) && wait_futex_subs_le(base_subs, 1000);
    long after_subs = read_futex_subs();
    if (lost_wakes == 0 && subs_ok) {
        printf("fork-wake-probe: pass killed-shared-wake (subs=%ld->%ld)\n", base_subs, after_subs);
        return 0;
    }
    printf("fork-wake-probe: FAIL killed-shared-wake lost_wakes=%d/24 subs=%ld->%ld\n",
           lost_wakes, base_subs, after_subs);
    return 1;
}

static int case_timeout_exit_wake(void) {
    long base_subs = read_futex_subs();
    uint32_t word = 0;

    struct timespec dl = mono_after_ms(50);
    struct simple_waiter_ctx wa = {.word = &word, .deadline = &dl};
    pthread_t ta;
    if (pthread_create(&ta, NULL, simple_futex_waiter, &wa) != 0) {
        printf("fork-wake-probe: FAIL timeout-exit-wake pthread_create A\n");
        return 1;
    }
    pthread_join(ta, NULL);
    if (atomic_load(&wa.rc) != -ETIMEDOUT) {
        printf("fork-wake-probe: FAIL timeout-exit-wake A returned %ld expected %d\n",
               atomic_load(&wa.rc), -ETIMEDOUT);
        return 1;
    }

    struct simple_waiter_ctx wb = {.word = &word, .deadline = NULL};
    pthread_t tb;
    if (pthread_create(&tb, NULL, simple_futex_waiter, &wb) != 0) {
        printf("fork-wake-probe: FAIL timeout-exit-wake pthread_create B\n");
        return 1;
    }
    wait_flag_ms(&wb.entered, 1000);
    sleep_ms(100);

    word = 1;
    ssize_t woken = futex_wake_raw(&word, 1);
    int b_woke = wait_flag_ms(&wb.done, 3000);
    if (!b_woke) {
        futex_wake_raw(&word, 16);
        wait_flag_ms(&wb.done, 2000);
    }
    pthread_join(tb, NULL);

    int race_lost = 0;
    for (int round = 0; round < 32; round++) {
        uint32_t rword = 0;
        struct timespec rdl = mono_after_ms(10);
        struct simple_waiter_ctx rwa = {.word = &rword, .deadline = &rdl};
        struct simple_waiter_ctx rwb = {.word = &rword, .deadline = NULL};
        pthread_t rta, rtb;
        if (pthread_create(&rta, NULL, simple_futex_waiter, &rwa) != 0 ||
            pthread_create(&rtb, NULL, simple_futex_waiter, &rwb) != 0) {
            printf("fork-wake-probe: FAIL timeout-exit-wake race pthread_create\n");
            return 1;
        }
        wait_flag_ms(&rwa.entered, 1000);
        wait_flag_ms(&rwb.entered, 1000);
        sleep_ms(10);
        rword = 1;
        ssize_t rw = futex_wake_raw(&rword, 1);
        sleep_ms(50);
        int a_timed_out = atomic_load(&rwa.done) && atomic_load(&rwa.rc) == -ETIMEDOUT;
        int b_still_blocked = !atomic_load(&rwb.done);
        if (rw == 1 && a_timed_out && b_still_blocked) {
            race_lost++;
        }
        futex_wake_raw(&rword, 16);
        pthread_join(rta, NULL);
        pthread_join(rtb, NULL);
    }

    int subs_ok = (base_subs >= 0) && wait_futex_subs_le(base_subs, 1000);
    long after_subs = read_futex_subs();
    if (woken == 1 && b_woke && atomic_load(&wb.rc) == 0 && race_lost == 0 && subs_ok) {
        printf("fork-wake-probe: pass timeout-exit-wake (subs=%ld->%ld)\n", base_subs, after_subs);
        return 0;
    }
    printf("fork-wake-probe: FAIL timeout-exit-wake woken=%zd b_woke=%d race_lost=%d subs=%ld->%ld\n",
           woken, b_woke, race_lost, base_subs, after_subs);
    return 1;
}

struct cond_case_ctx {
    pthread_mutex_t mu;
    pthread_cond_t cv;
    int flag;
    atomic_int b_entered;
    atomic_int b_done;
    atomic_int a_rc;
};

static void *cond_timed_a(void *arg) {
    struct cond_case_ctx *c = (struct cond_case_ctx *)arg;
    pthread_mutex_lock(&c->mu);
    struct timespec dl = mono_after_ms(50);
    int rc = pthread_cond_timedwait(&c->cv, &c->mu, &dl);
    atomic_store(&c->a_rc, rc);
    pthread_mutex_unlock(&c->mu);
    return NULL;
}

static void *cond_wait_b(void *arg) {
    struct cond_case_ctx *c = (struct cond_case_ctx *)arg;
    pthread_mutex_lock(&c->mu);
    atomic_store(&c->b_entered, 1);
    while (!c->flag) {
        pthread_cond_wait(&c->cv, &c->mu);
    }
    pthread_mutex_unlock(&c->mu);
    atomic_store(&c->b_done, 1);
    return NULL;
}

static int case_cond_timedwait_signal(void) {
    long base_subs = read_futex_subs();
    struct cond_case_ctx c;
    memset(&c, 0, sizeof(c));
    pthread_mutex_init(&c.mu, NULL);
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_init(&c.cv, &attr);
    pthread_condattr_destroy(&attr);

    pthread_t ta, tb;
    if (pthread_create(&ta, NULL, cond_timed_a, &c) != 0) {
        printf("fork-wake-probe: FAIL cond-timedwait-signal pthread_create A\n");
        return 1;
    }
    pthread_join(ta, NULL);
    if (atomic_load(&c.a_rc) != ETIMEDOUT) {
        printf("fork-wake-probe: FAIL cond-timedwait-signal A rc=%d expected %d\n",
               atomic_load(&c.a_rc), ETIMEDOUT);
        return 1;
    }

    if (pthread_create(&tb, NULL, cond_wait_b, &c) != 0) {
        printf("fork-wake-probe: FAIL cond-timedwait-signal pthread_create B\n");
        return 1;
    }
    wait_flag_ms(&c.b_entered, 1000);
    sleep_ms(100);

    pthread_mutex_lock(&c.mu);
    c.flag = 1;
    pthread_cond_signal(&c.cv);
    pthread_mutex_unlock(&c.mu);

    int b_woke = wait_flag_ms(&c.b_done, 3000);
    if (!b_woke) {
        pthread_mutex_lock(&c.mu);
        c.flag = 1;
        pthread_cond_broadcast(&c.cv);
        pthread_mutex_unlock(&c.mu);
        wait_flag_ms(&c.b_done, 2000);
    }
    pthread_join(tb, NULL);
    pthread_cond_destroy(&c.cv);
    pthread_mutex_destroy(&c.mu);

    int subs_ok = (base_subs >= 0) && wait_futex_subs_le(base_subs, 1000);
    long after_subs = read_futex_subs();
    if (b_woke && subs_ok) {
        printf("fork-wake-probe: pass cond-timedwait-signal (subs=%ld->%ld)\n", base_subs, after_subs);
        return 0;
    }
    printf("fork-wake-probe: FAIL cond-timedwait-signal b_woke=%d subs=%ld->%ld\n",
           b_woke, base_subs, after_subs);
    return 1;
}

static int case_killed_private_reuse(void) {
    long base_subs = read_futex_subs();
    for (int round = 0; round < 8; round++) {
        priv_reuse_word = 0;
        int pfd[2];
        if (pipe(pfd) != 0) {
            printf("fork-wake-probe: FAIL killed-private-reuse pipe\n");
            return 1;
        }
        fflush(stdout);
        pid_t victim = fork();
        if (victim < 0) {
            close(pfd[0]);
            close(pfd[1]);
            printf("fork-wake-probe: FAIL killed-private-reuse fork victim\n");
            return 1;
        }
        if (victim == 0) {
            close(pfd[0]);
            char b = 1;
            if (write(pfd[1], &b, 1) != 1) {
                _exit(2);
            }
            close(pfd[1]);
            futex_wait_raw(&priv_reuse_word, 0, NULL);
            _exit(0);
        }
        close(pfd[1]);
        char b = 0;
        if (read(pfd[0], &b, 1) != 1) {
            kill(victim, SIGKILL);
            waitpid(victim, NULL, 0);
            close(pfd[0]);
            printf("fork-wake-probe: FAIL killed-private-reuse victim handshake\n");
            return 1;
        }
        close(pfd[0]);
        sleep_ms(80);
        kill(victim, SIGKILL);
        waitpid(victim, NULL, 0);

        fflush(stdout);
        pid_t runner = fork();
        if (runner < 0) {
            printf("fork-wake-probe: FAIL killed-private-reuse fork runner\n");
            return 1;
        }
        if (runner == 0) {
            alarm(10);
            priv_reuse_word = 0;
            struct simple_waiter_ctx wb = {.word = &priv_reuse_word, .deadline = NULL};
            pthread_t tb;
            if (pthread_create(&tb, NULL, simple_futex_waiter, &wb) != 0) {
                _exit(11);
            }
            wait_flag_ms(&wb.entered, 1000);
            sleep_ms(80);
            priv_reuse_word = 1;
            ssize_t woken = futex_wake_raw(&priv_reuse_word, 1);
            int woke = wait_flag_ms(&wb.done, 3000);
            if (!woke) {
                futex_wake_raw(&priv_reuse_word, 16);
                wait_flag_ms(&wb.done, 1000);
            }
            pthread_join(tb, NULL);
            if (woken != 1 || !woke || atomic_load(&wb.rc) != 0) {
                _exit(12);
            }
            _exit(0);
        }
        int status = 0;
        if (waitpid(runner, &status, 0) != runner || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            printf("fork-wake-probe: FAIL killed-private-reuse round=%d status=0x%x\n", round, status);
            return 1;
        }
    }

    int subs_ok = (base_subs >= 0) && wait_futex_subs_le(base_subs, 1000);
    long after_subs = read_futex_subs();
    if (subs_ok) {
        printf("fork-wake-probe: pass killed-private-reuse (subs=%ld->%ld)\n", base_subs, after_subs);
        return 0;
    }
    printf("fork-wake-probe: FAIL killed-private-reuse leaked futex_subcribers=%ld->%ld\n",
           base_subs, after_subs);
    return 1;
}

int main(void) {
    alarm(60);
    setvbuf(stdout, NULL, _IOLBF, 0);
    int failed = 0;
    failed |= case_fork_basic();
    failed |= case_wake_signal_race();
    failed |= case_killed_shared_wake();
    failed |= case_timeout_exit_wake();
    failed |= case_cond_timedwait_signal();
    failed |= case_killed_private_reuse();
    if (failed) {
        printf("fork-wake-probe: FAIL\n");
        return 1;
    }
    printf("fork-wake-probe: PASS\n");
    return 0;
}
