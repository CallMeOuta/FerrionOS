#include <assert.h>
#include <errno.h>
#include <fcntl.h>
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

#define PAGE_BYTES 4096
#define KERNEL_ROUNDS 64
#define RELIBC_ROUNDS 96
#define RELIBC_WORKERS 8
#define MAX_VICTIM_ITERS 4000000U
#define MAX_READY_POLLS 500000U
#define MAX_WAIT_RETRIES 64
#define JITTER_MOD 8
#define JITTER_STEP_NS 150000L
#define RELIBC_JITTER_MOD 64
#define BENCH_WARMUP 10000U
#define BENCH_BATCHES 64
#define BENCH_ITERS 2000U

struct shared_state {
    atomic_uint ready;
    atomic_uint kill_sent;
    atomic_uint eintr_count;
    atomic_uint post_kill_syscalls;
};

struct relibc_sync {
    atomic_uint ready;
    atomic_uint go;
};

static uint64_t monotonic_ns(void)
{
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static int wait_child(pid_t pid, int *status_out)
{
    for (int i = 0; i < MAX_WAIT_RETRIES; i++) {
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

static void sleep_ns(long ns)
{
    if (ns <= 0) {
        return;
    }
    struct timespec req;
    req.tv_sec = 0;
    req.tv_nsec = ns;
    (void)nanosleep(&req, NULL);
}

static int run_kernel_case(int scheme_mode, const char *label)
{
    struct shared_state *sh = mmap(
        NULL,
        PAGE_BYTES,
        PROT_READ | PROT_WRITE,
        MAP_SHARED | MAP_ANONYMOUS,
        -1,
        0
    );
    if (sh == MAP_FAILED) {
        fprintf(stderr, "sigkill-exit-probe: mmap failed errno=%d\n", errno);
        return 1;
    }

    unsigned int eintr_rounds = 0;
    unsigned int eintr_total = 0;
    unsigned int post_rounds = 0;
    unsigned int post_total = 0;
    unsigned int kill_err_rounds = 0;

    for (int round = 0; round < KERNEL_ROUNDS; round++) {
        atomic_store_explicit(&sh->ready, 0, memory_order_relaxed);
        atomic_store_explicit(&sh->kill_sent, 0, memory_order_relaxed);
        atomic_store_explicit(&sh->eintr_count, 0, memory_order_relaxed);
        atomic_store_explicit(&sh->post_kill_syscalls, 0, memory_order_relaxed);

        pid_t pid = fork();
        if (pid < 0) {
            fprintf(stderr, "sigkill-exit-probe: fork failed errno=%d\n", errno);
            munmap(sh, PAGE_BYTES);
            return 1;
        }
        if (pid == 0) {
            int null_fd = -1;
            if (scheme_mode) {
                null_fd = open("/dev/null", O_RDONLY);
                if (null_fd < 0) {
                    _exit(99);
                }
            }
            atomic_store_explicit(&sh->ready, 1, memory_order_release);
            unsigned int seen_kill = 0;
            unsigned char byte = 0;
            struct timespec ts;
            for (unsigned int i = 0; i < MAX_VICTIM_ITERS; i++) {
                if (scheme_mode) {
                    ssize_t r = read(null_fd, &byte, 1);
                    if (r < 0 && errno == EINTR) {
                        atomic_fetch_add_explicit(&sh->eintr_count, 1, memory_order_relaxed);
                    }
                } else {
                    (void)clock_gettime(CLOCK_MONOTONIC, &ts);
                }
                if (atomic_load_explicit(&sh->kill_sent, memory_order_acquire) != 0) {
                    seen_kill++;
                    if (seen_kill >= 2) {
                        atomic_fetch_add_explicit(&sh->post_kill_syscalls, 1, memory_order_relaxed);
                    }
                }
            }
            if (null_fd >= 0) {
                close(null_fd);
            }
            _exit(0);
        }

        unsigned int ready = 0;
        for (unsigned int p = 0; p < MAX_READY_POLLS; p++) {
            if (atomic_load_explicit(&sh->ready, memory_order_acquire) != 0) {
                ready = 1;
                break;
            }
            sched_yield();
        }
        if (!ready) {
            (void)kill(pid, SIGKILL);
            int st = 0;
            (void)wait_child(pid, &st);
            fprintf(stderr, "sigkill-exit-probe: %s child never ready on round %d\n", label, round);
            munmap(sh, PAGE_BYTES);
            return 1;
        }

        sleep_ns((long)(round % JITTER_MOD) * JITTER_STEP_NS);
        if (kill(pid, SIGKILL) != 0) {
            kill_err_rounds++;
        }
        atomic_store_explicit(&sh->kill_sent, 1, memory_order_release);

        int st = 0;
        if (wait_child(pid, &st) != 0) {
            fprintf(stderr, "sigkill-exit-probe: %s waitpid failed round %d errno=%d\n", label, round, errno);
            munmap(sh, PAGE_BYTES);
            return 1;
        }

        unsigned int ec = atomic_load_explicit(&sh->eintr_count, memory_order_acquire);
        unsigned int pc = atomic_load_explicit(&sh->post_kill_syscalls, memory_order_acquire);
        if (ec > 0) {
            eintr_rounds++;
            eintr_total += ec;
        }
        if (pc > 0) {
            post_rounds++;
            post_total += pc;
        }
    }

    munmap(sh, PAGE_BYTES);
    printf(
        "sigkill-exit-probe: kernel %s rounds=%d eintr_rounds=%u eintr_total=%u post_kill_rounds=%u post_kill_total=%u kill_err=%u\n",
        label,
        KERNEL_ROUNDS,
        eintr_rounds,
        eintr_total,
        post_rounds,
        post_total,
        kill_err_rounds
    );
    fflush(stdout);
    return (eintr_rounds == 0 && post_rounds == 0 && kill_err_rounds == 0) ? 0 : 1;
}

static void *relibc_worker(void *arg)
{
    struct relibc_sync *sync = (struct relibc_sync *)arg;
    atomic_fetch_add_explicit(&sync->ready, 1, memory_order_release);
    for (unsigned int i = 0; i < MAX_READY_POLLS; i++) {
        if (atomic_load_explicit(&sync->go, memory_order_acquire) != 0) {
            break;
        }
        sched_yield();
    }
    pthread_exit(NULL);
    return NULL;
}

static int run_relibc_case(void)
{
    unsigned int exit7_rounds = 0;
    unsigned int sigill_rounds = 0;
    unsigned int other_rounds = 0;
    int first_bad = 0;

    for (int round = 0; round < RELIBC_ROUNDS; round++) {
        pid_t pid = fork();
        if (pid < 0) {
            fprintf(stderr, "sigkill-exit-probe: relibc fork failed errno=%d\n", errno);
            return 1;
        }
        if (pid == 0) {
            struct relibc_sync sync;
            atomic_store_explicit(&sync.ready, 0, memory_order_relaxed);
            atomic_store_explicit(&sync.go, 0, memory_order_relaxed);

            pthread_t tids[RELIBC_WORKERS];
            for (int w = 0; w < RELIBC_WORKERS; w++) {
                int rc = pthread_create(&tids[w], NULL, relibc_worker, &sync);
                if (rc != 0) {
                    _exit(98);
                }
            }
            for (unsigned int p = 0; p < MAX_READY_POLLS; p++) {
                if (atomic_load_explicit(&sync.ready, memory_order_acquire) >= RELIBC_WORKERS) {
                    break;
                }
                sched_yield();
            }
            atomic_store_explicit(&sync.go, 1, memory_order_release);
            struct timespec ts;
            int spins = round % RELIBC_JITTER_MOD;
            for (int s = 0; s < spins; s++) {
                (void)clock_gettime(CLOCK_MONOTONIC, &ts);
            }
            exit(7);
        }

        int st = 0;
        if (wait_child(pid, &st) != 0) {
            fprintf(stderr, "sigkill-exit-probe: relibc waitpid failed round %d errno=%d\n", round, errno);
            return 1;
        }
        if (WIFEXITED(st) && WEXITSTATUS(st) == 7) {
            exit7_rounds++;
        } else if (WIFSIGNALED(st) && WTERMSIG(st) == SIGILL) {
            sigill_rounds++;
            if (first_bad == 0) {
                first_bad = st;
            }
        } else {
            other_rounds++;
            if (first_bad == 0) {
                first_bad = st;
            }
        }
    }

    printf(
        "sigkill-exit-probe: relibc rounds=%d exit7=%u sigill=%u other=%u first_bad=0x%x\n",
        RELIBC_ROUNDS,
        exit7_rounds,
        sigill_rounds,
        other_rounds,
        (unsigned int)first_bad
    );
    fflush(stdout);
    return (exit7_rounds == RELIBC_ROUNDS && sigill_rounds == 0 && other_rounds == 0) ? 0 : 1;
}

static int cmp_u64(const void *a, const void *b)
{
    uint64_t va = *(const uint64_t *)a;
    uint64_t vb = *(const uint64_t *)b;
    if (va < vb) {
        return -1;
    }
    if (va > vb) {
        return 1;
    }
    return 0;
}

static int run_bench(void)
{
    struct timespec ts;
    for (unsigned int i = 0; i < BENCH_WARMUP; i++) {
        (void)clock_gettime(CLOCK_MONOTONIC, &ts);
    }

    uint64_t samples_tenth_ns[BENCH_BATCHES];
    for (int b = 0; b < BENCH_BATCHES; b++) {
        uint64_t t0 = monotonic_ns();
        for (unsigned int i = 0; i < BENCH_ITERS; i++) {
            (void)clock_gettime(CLOCK_MONOTONIC, &ts);
        }
        uint64_t t1 = monotonic_ns();
        uint64_t elapsed = (t1 >= t0) ? (t1 - t0) : 0;
        samples_tenth_ns[b] = (elapsed * 10ULL) / BENCH_ITERS;
    }

    qsort(samples_tenth_ns, BENCH_BATCHES, sizeof(uint64_t), cmp_u64);
    uint64_t min_v = samples_tenth_ns[0];
    uint64_t med_v = samples_tenth_ns[BENCH_BATCHES / 2];
    uint64_t p95_v = samples_tenth_ns[(BENCH_BATCHES * 95) / 100];
    uint64_t max_v = samples_tenth_ns[BENCH_BATCHES - 1];
    printf(
        "sigkill-exit-probe: bench batches=%d iters=%u min_ns=%llu.%llu median_ns=%llu.%llu p95_ns=%llu.%llu max_ns=%llu.%llu\n",
        BENCH_BATCHES,
        BENCH_ITERS,
        (unsigned long long)(min_v / 10ULL),
        (unsigned long long)(min_v % 10ULL),
        (unsigned long long)(med_v / 10ULL),
        (unsigned long long)(med_v % 10ULL),
        (unsigned long long)(p95_v / 10ULL),
        (unsigned long long)(p95_v % 10ULL),
        (unsigned long long)(max_v / 10ULL),
        (unsigned long long)(max_v % 10ULL)
    );
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv)
{
    const char *mode = (argc >= 2) ? argv[1] : "all";
    if (strcmp(mode, "kernel") == 0) {
        int rc1 = run_kernel_case(1, "scheme-eintr");
        int rc2 = run_kernel_case(0, "syscall-spin");
        return (rc1 == 0 && rc2 == 0) ? 0 : 1;
    }
    if (strcmp(mode, "relibc") == 0) {
        return run_relibc_case();
    }
    if (strcmp(mode, "bench") == 0) {
        return run_bench();
    }
    if (strcmp(mode, "all") == 0) {
        int rc1 = run_kernel_case(1, "scheme-eintr");
        int rc2 = run_kernel_case(0, "syscall-spin");
        int rc3 = run_relibc_case();
        return (rc1 == 0 && rc2 == 0 && rc3 == 0) ? 0 : 1;
    }
    fprintf(stderr, "usage: sigkill-exit-probe [kernel|relibc|bench|all]\n");
    return 2;
}
