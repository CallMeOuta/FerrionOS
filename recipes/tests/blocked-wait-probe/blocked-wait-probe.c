#define _POSIX_C_SOURCE 200809L
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    PARTA_ROUNDS = 50,
    PARTA_CONTROL_ROUNDS = 10,
    PARTA_EARLY_WAIT_MS = 200,
    PARTA_WAKE_DELAY_MS = 5,
    PARTA_CONTROL_WAIT_MS = 20,
    PARTA_FALLBACK_MS = 450,
    PARTA_WATCHDOG_S = 60,
    PARTA2_ROUNDS = 50,
    PARTA2_CONTROL_ROUNDS = 10,
    PARTA2_WAIT_MS = 20,
    PARTA2_BROADCAST_MS = 50,
    PARTA2_FALLBACK_MS = 450,
    PARTA2_WATCHDOG_S = 60,
    PARTB_CONTROL_ROUNDS = 200,
    PARTB_CONTROL_WAIT_MS = 5,
    PARTB_CONTROL_CHILD_MS = 15,
    PARTB_CONTROL_WATCHDOG_S = 30,
    PART_RUN_SECS = 10,
    PART_WATCHDOG_S = 22,
    MAX_SWEEP_WORKERS = 2,
    SWEEP_MAX_US = 120,
    SWEEP_STEP_US = 2,
    NUM_LEAD_BUCKETS = (SWEEP_MAX_US / SWEEP_STEP_US) + 1,
    ARM_OFFSET_US = 2000,
    EARLY_WAKE_US = 200,
    UNBLOCK_THRESHOLD_MS = 20,
    MAX_PART_ROUNDS = 20000,
    MAX_FORK_CHILDREN = 20000,
    MAX_PUMPER_YIELDS = 50000000,
    MAX_HELPER_LOOPS = 50000000,
    MAX_SPIN_ITERS = 20000000,
    MAX_DIR_ENTRIES = 1024,
    TSC_CAL_MS = 50,
    NVME_BLOCK_BYTES = 4096,
    NVME_LBA_SPAN = 256,
    ZERO_CHECK_BYTES = 64,
    RAND_CHECK_BYTES = 32,
    MAX_PROBE_IO_RETRIES = 16,
    NS_PER_SEC = 1000000000LL,
    NS_PER_MS = 1000000LL,
    NS_PER_US = 1000LL
};

static double g_ticks_per_us = 1000.0;

static uint64_t read_tsc(void) {
#if defined(__x86_64__)
    uint32_t lo = 0;
    uint32_t hi = 0;
    __asm__ __volatile__("rdtsc" : "=a"(lo), "=d"(hi));
    return ((uint64_t)hi << 32) | (uint64_t)lo;
#else
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * (uint64_t)NS_PER_SEC + (uint64_t)ts.tv_nsec;
#endif
}

static int64_t monotonic_ns(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (int64_t)ts.tv_sec * NS_PER_SEC + (int64_t)ts.tv_nsec;
}

static struct timespec ns_to_timespec(int64_t ns) {
    struct timespec ts;
    ts.tv_sec = (time_t)(ns / NS_PER_SEC);
    ts.tv_nsec = (long)(ns % NS_PER_SEC);
    return ts;
}

static int init_monotonic_cond(pthread_cond_t *cond) {
    pthread_condattr_t attr;
    if (pthread_condattr_init(&attr) != 0) {
        return -1;
    }
    int rc = pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    if (rc == 0) {
        rc = pthread_cond_init(cond, &attr);
    }
    (void)pthread_condattr_destroy(&attr);
    return rc == 0 ? 0 : -1;
}

static int calibrate_tsc(void) {
    int64_t t0 = monotonic_ns();
    uint64_t c0 = read_tsc();
    struct timespec ts = {0, TSC_CAL_MS * NS_PER_MS};
    (void)nanosleep(&ts, NULL);
    uint64_t c1 = read_tsc();
    int64_t t1 = monotonic_ns();
    int64_t dns = t1 - t0;
    uint64_t dtsc = c1 - c0;
    if (dns <= 0 || dtsc == 0) {
        return -1;
    }
    g_ticks_per_us = ((double)dtsc * 1000.0) / (double)dns;
    return 0;
}

static void spin_until_tsc(uint64_t target) {
    for (int i = 0; i < MAX_SPIN_ITERS; i++) {
        if (read_tsc() >= target) {
            break;
        }
    }
}

static int check_unpartitioned_disk(const char *disk_path) {
    if (strncmp(disk_path, "/scheme/disk.", 13) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR invalid disk path %s\n", disk_path);
        return -1;
    }
    const char *slash = strrchr(disk_path, '/');
    if (slash == NULL || slash == disk_path || slash[1] == '\0') {
        fprintf(stderr, "BLOCKED-WAIT-ERROR missing unit in %s\n", disk_path);
        return -1;
    }
    size_t dir_len = (size_t)(slash - disk_path);
    if (dir_len >= 256) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR disk path too long\n");
        return -1;
    }
    char dir_path[256];
    memcpy(dir_path, disk_path, dir_len);
    dir_path[dir_len] = '\0';
    const char *unit = slash + 1;

    DIR *dir = opendir(dir_path);
    if (dir == NULL) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR opendir %s failed errno=%d\n", dir_path, errno);
        return -1;
    }
    int found_unit = 0;
    for (int i = 0; i < MAX_DIR_ENTRIES; i++) {
        struct dirent *ent = readdir(dir);
        if (ent == NULL) {
            break;
        }
        const char *name = ent->d_name;
        if (strcmp(name, ".") == 0 || strcmp(name, "..") == 0) {
            continue;
        }
        if (strchr(name, 'p') != NULL) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR refusing partitioned disk %s entry %s\n", dir_path, name);
            (void)closedir(dir);
            return -1;
        }
        if (strcmp(name, unit) == 0) {
            found_unit = 1;
        }
    }
    (void)closedir(dir);
    if (!found_unit) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR unit %s not found in %s\n", unit, dir_path);
        return -1;
    }
    return 0;
}

typedef struct {
    pthread_mutex_t mtx;
    pthread_cond_t cond;
    int pfd[2];
    atomic_int mode;
    atomic_int round_req;
    atomic_int round_ack;
    atomic_int read_done;
    atomic_int stop;
    atomic_int_fast64_t arm_ns;
} PartAState;

enum {
    PARTA_MODE_EARLY = 1,
    PARTA_MODE_CONTROL = 2
};

static void *parta_helper(void *arg) {
    PartAState *st = (PartAState *)arg;
    int done_id = 0;
    for (int loop = 0; loop < MAX_HELPER_LOOPS && !atomic_load(&st->stop); loop++) {
        int req_id = atomic_load(&st->round_req);
        if (req_id <= done_id) {
            struct timespec ts = {0, 1 * NS_PER_MS};
            (void)nanosleep(&ts, NULL);
            continue;
        }
        int mode = atomic_load(&st->mode);
        if (mode == PARTA_MODE_EARLY) {
            (void)pthread_mutex_lock(&st->mtx);
            (void)pthread_mutex_unlock(&st->mtx);
            struct timespec ws = {0, PARTA_WAKE_DELAY_MS * NS_PER_MS};
            (void)nanosleep(&ws, NULL);
            (void)pthread_cond_signal(&st->cond);
        }
        int64_t base_ns = atomic_load(&st->arm_ns);
        int64_t fb_ns = base_ns + (int64_t)PARTA_FALLBACK_MS * NS_PER_MS;
        for (int step = 0; step < 500 && !atomic_load(&st->stop); step++) {
            if (atomic_load(&st->read_done)) {
                break;
            }
            if (monotonic_ns() >= fb_ns) {
                char b = 1;
                (void)write(st->pfd[1], &b, 1);
                break;
            }
            struct timespec ts = {0, 2 * NS_PER_MS};
            (void)nanosleep(&ts, NULL);
        }
        done_id = req_id;
        atomic_store(&st->round_ack, done_id);
    }
    return NULL;
}

static int run_part_a(void) {
    PartAState st;
    memset(&st, 0, sizeof(st));
    if (pthread_mutex_init(&st.mtx, NULL) != 0 || init_monotonic_cond(&st.cond) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR parta init failed\n");
        return 2;
    }
    if (pipe(st.pfd) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR parta pipe failed errno=%d\n", errno);
        return 2;
    }
    atomic_init(&st.mode, 0);
    atomic_init(&st.round_req, 0);
    atomic_init(&st.round_ack, 0);
    atomic_init(&st.read_done, 0);
    atomic_init(&st.stop, 0);
    atomic_init(&st.arm_ns, 0);

    pthread_t helper_tid;
    if (pthread_create(&helper_tid, NULL, parta_helper, &st) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR parta pthread_create failed\n");
        return 2;
    }

    int early_ok = 0;
    int early_eintr = 0;
    int early_data = 0;
    int early_other = 0;
    int64_t early_min_ms = -1;
    int64_t early_max_ms = -1;

    int ctrl_timedout = 0;
    int ctrl_eintr = 0;
    int ctrl_data = 0;
    int ctrl_other = 0;
    int64_t ctrl_min_ms = -1;
    int64_t ctrl_max_ms = -1;

    int total_rounds = PARTA_ROUNDS + PARTA_CONTROL_ROUNDS;
    for (int r = 0; r < total_rounds; r++) {
        int id = r + 1;
        int is_early = (r < PARTA_ROUNDS);
        int wait_ms = is_early ? PARTA_EARLY_WAIT_MS : PARTA_CONTROL_WAIT_MS;
        (void)pthread_mutex_lock(&st.mtx);
        int64_t arm_ns = monotonic_ns();
        struct timespec abstime = ns_to_timespec(arm_ns + (int64_t)wait_ms * NS_PER_MS);
        atomic_store(&st.arm_ns, arm_ns);
        atomic_store(&st.read_done, 0);
        atomic_store(&st.mode, is_early ? PARTA_MODE_EARLY : PARTA_MODE_CONTROL);
        atomic_store(&st.round_req, id);
        int wrc = pthread_cond_timedwait(&st.cond, &st.mtx, &abstime);
        (void)pthread_mutex_unlock(&st.mtx);

        char buf = 0;
        ssize_t n = read(st.pfd[0], &buf, 1);
        int saved_errno = errno;
        int64_t elapsed_ms = (monotonic_ns() - arm_ns) / NS_PER_MS;
        atomic_store(&st.read_done, 1);

        if (is_early) {
            if (wrc == 0) {
                early_ok++;
            }
            if (n == -1 && saved_errno == EINTR) {
                early_eintr++;
            } else if (n == 1) {
                early_data++;
            } else {
                early_other++;
            }
            if (early_min_ms < 0 || elapsed_ms < early_min_ms) {
                early_min_ms = elapsed_ms;
            }
            if (elapsed_ms > early_max_ms) {
                early_max_ms = elapsed_ms;
            }
        } else {
            if (wrc == ETIMEDOUT) {
                ctrl_timedout++;
            }
            if (n == -1 && saved_errno == EINTR) {
                ctrl_eintr++;
            } else if (n == 1) {
                ctrl_data++;
            } else {
                ctrl_other++;
            }
            if (ctrl_min_ms < 0 || elapsed_ms < ctrl_min_ms) {
                ctrl_min_ms = elapsed_ms;
            }
            if (elapsed_ms > ctrl_max_ms) {
                ctrl_max_ms = elapsed_ms;
            }
        }

        for (int wait_it = 0; wait_it < 2000; wait_it++) {
            if (atomic_load(&st.round_ack) >= id) {
                break;
            }
            struct timespec ts = {0, 1 * NS_PER_MS};
            (void)nanosleep(&ts, NULL);
        }
        int fl = fcntl(st.pfd[0], F_GETFL, 0);
        if (fl >= 0) {
            (void)fcntl(st.pfd[0], F_SETFL, fl | O_NONBLOCK);
            char drain[16];
            for (int d = 0; d < 16; d++) {
                if (read(st.pfd[0], drain, sizeof(drain)) <= 0) {
                    break;
                }
            }
            (void)fcntl(st.pfd[0], F_SETFL, fl);
        }
    }

    atomic_store(&st.stop, 1);
    (void)pthread_join(helper_tid, NULL);
    (void)close(st.pfd[0]);
    (void)close(st.pfd[1]);
    (void)pthread_cond_destroy(&st.cond);
    (void)pthread_mutex_destroy(&st.mtx);

    printf("BLOCKED-WAIT-PARTA early_rounds=%d early_ok=%d early_eintr=%d early_data=%d early_other=%d early_min_ms=%lld early_max_ms=%lld ctrl_rounds=%d ctrl_timedout=%d ctrl_eintr=%d ctrl_data=%d ctrl_other=%d ctrl_min_ms=%lld ctrl_max_ms=%lld\n",
           PARTA_ROUNDS, early_ok, early_eintr, early_data, early_other,
           (long long)early_min_ms, (long long)early_max_ms,
           PARTA_CONTROL_ROUNDS, ctrl_timedout, ctrl_eintr, ctrl_data, ctrl_other,
           (long long)ctrl_min_ms, (long long)ctrl_max_ms);
    fflush(stdout);
    return 0;
}

typedef struct {
    pthread_mutex_t mtx;
    pthread_cond_t cond;
    int pfd[2];
    atomic_int do_bcast;
    atomic_int round_req;
    atomic_int round_ack;
    atomic_int read_done;
    atomic_int stop;
    atomic_int_fast64_t read_start_ns;
} PartA2State;

static void *parta2_helper(void *arg) {
    PartA2State *st = (PartA2State *)arg;
    int done_id = 0;
    for (int loop = 0; loop < MAX_HELPER_LOOPS && !atomic_load(&st->stop); loop++) {
        int req_id = atomic_load(&st->round_req);
        if (req_id <= done_id) {
            struct timespec ts = {0, 1 * NS_PER_MS};
            (void)nanosleep(&ts, NULL);
            continue;
        }
        int64_t base_ns = atomic_load(&st->read_start_ns);
        int do_bcast = atomic_load(&st->do_bcast);
        int bcast_sent = 0;
        int64_t bcast_ns = base_ns + (int64_t)PARTA2_BROADCAST_MS * NS_PER_MS;
        int64_t fb_ns = base_ns + (int64_t)PARTA2_FALLBACK_MS * NS_PER_MS;
        for (int step = 0; step < 500 && !atomic_load(&st->stop); step++) {
            if (atomic_load(&st->read_done)) {
                break;
            }
            int64_t now_ns = monotonic_ns();
            if (do_bcast && !bcast_sent && now_ns >= bcast_ns) {
                (void)pthread_cond_broadcast(&st->cond);
                bcast_sent = 1;
            }
            if (now_ns >= fb_ns) {
                char b = 1;
                (void)write(st->pfd[1], &b, 1);
                break;
            }
            struct timespec ts = {0, 2 * NS_PER_MS};
            (void)nanosleep(&ts, NULL);
        }
        done_id = req_id;
        atomic_store(&st->round_ack, done_id);
    }
    return NULL;
}

static int run_part_a2(void) {
    PartA2State st;
    memset(&st, 0, sizeof(st));
    if (pthread_mutex_init(&st.mtx, NULL) != 0 || init_monotonic_cond(&st.cond) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR parta2 init failed\n");
        return 2;
    }
    if (pipe(st.pfd) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR parta2 pipe failed errno=%d\n", errno);
        return 2;
    }
    atomic_init(&st.do_bcast, 0);
    atomic_init(&st.round_req, 0);
    atomic_init(&st.round_ack, 0);
    atomic_init(&st.read_done, 0);
    atomic_init(&st.stop, 0);
    atomic_init(&st.read_start_ns, 0);

    pthread_t helper_tid;
    if (pthread_create(&helper_tid, NULL, parta2_helper, &st) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR parta2 pthread_create failed\n");
        return 2;
    }

    int bcast_timedout = 0;
    int bcast_eintr = 0;
    int bcast_data = 0;
    int bcast_other = 0;
    int64_t bcast_min_ms = -1;
    int64_t bcast_max_ms = -1;

    int ctrl_timedout = 0;
    int ctrl_eintr = 0;
    int ctrl_data = 0;
    int ctrl_other = 0;
    int64_t ctrl_min_ms = -1;
    int64_t ctrl_max_ms = -1;

    int total_rounds = PARTA2_ROUNDS + PARTA2_CONTROL_ROUNDS;
    for (int r = 0; r < total_rounds; r++) {
        int id = r + 1;
        int is_bcast = (r < PARTA2_ROUNDS);
        (void)pthread_mutex_lock(&st.mtx);
        struct timespec abstime = ns_to_timespec(monotonic_ns() + (int64_t)PARTA2_WAIT_MS * NS_PER_MS);
        int wrc = pthread_cond_timedwait(&st.cond, &st.mtx, &abstime);
        (void)pthread_mutex_unlock(&st.mtx);

        int64_t read_start_ns = monotonic_ns();
        atomic_store(&st.read_start_ns, read_start_ns);
        atomic_store(&st.read_done, 0);
        atomic_store(&st.do_bcast, is_bcast ? 1 : 0);
        atomic_store(&st.round_req, id);

        char buf = 0;
        ssize_t n = read(st.pfd[0], &buf, 1);
        int saved_errno = errno;
        int64_t elapsed_ms = (monotonic_ns() - read_start_ns) / NS_PER_MS;
        atomic_store(&st.read_done, 1);

        if (is_bcast) {
            if (wrc == ETIMEDOUT) {
                bcast_timedout++;
            }
            if (n == -1 && saved_errno == EINTR) {
                bcast_eintr++;
            } else if (n == 1) {
                bcast_data++;
            } else {
                bcast_other++;
            }
            if (bcast_min_ms < 0 || elapsed_ms < bcast_min_ms) {
                bcast_min_ms = elapsed_ms;
            }
            if (elapsed_ms > bcast_max_ms) {
                bcast_max_ms = elapsed_ms;
            }
        } else {
            if (wrc == ETIMEDOUT) {
                ctrl_timedout++;
            }
            if (n == -1 && saved_errno == EINTR) {
                ctrl_eintr++;
            } else if (n == 1) {
                ctrl_data++;
            } else {
                ctrl_other++;
            }
            if (ctrl_min_ms < 0 || elapsed_ms < ctrl_min_ms) {
                ctrl_min_ms = elapsed_ms;
            }
            if (elapsed_ms > ctrl_max_ms) {
                ctrl_max_ms = elapsed_ms;
            }
        }

        for (int wait_it = 0; wait_it < 2000; wait_it++) {
            if (atomic_load(&st.round_ack) >= id) {
                break;
            }
            struct timespec ts = {0, 1 * NS_PER_MS};
            (void)nanosleep(&ts, NULL);
        }
        int fl = fcntl(st.pfd[0], F_GETFL, 0);
        if (fl >= 0) {
            (void)fcntl(st.pfd[0], F_SETFL, fl | O_NONBLOCK);
            char drain[16];
            for (int d = 0; d < 16; d++) {
                if (read(st.pfd[0], drain, sizeof(drain)) <= 0) {
                    break;
                }
            }
            (void)fcntl(st.pfd[0], F_SETFL, fl);
        }
    }

    atomic_store(&st.stop, 1);
    (void)pthread_join(helper_tid, NULL);
    (void)close(st.pfd[0]);
    (void)close(st.pfd[1]);
    (void)pthread_cond_destroy(&st.cond);
    (void)pthread_mutex_destroy(&st.mtx);

    printf("BLOCKED-WAIT-PARTA2 bcast_rounds=%d bcast_timedout=%d bcast_eintr=%d bcast_data=%d bcast_other=%d bcast_min_ms=%lld bcast_max_ms=%lld ctrl_rounds=%d ctrl_timedout=%d ctrl_eintr=%d ctrl_data=%d ctrl_other=%d ctrl_min_ms=%lld ctrl_max_ms=%lld\n",
           PARTA2_ROUNDS, bcast_timedout, bcast_eintr, bcast_data, bcast_other,
           (long long)bcast_min_ms, (long long)bcast_max_ms,
           PARTA2_CONTROL_ROUNDS, ctrl_timedout, ctrl_eintr, ctrl_data, ctrl_other,
           (long long)ctrl_min_ms, (long long)ctrl_max_ms);
    fflush(stdout);
    return 0;
}

enum {
    SWEEP_TARGET_UDS = 1,
    SWEEP_TARGET_NVME = 2,
    SWEEP_TARGET_PROCMGR = 3
};

typedef struct {
    int target;
    int num_workers;
    int enable_pumper;
    int enable_forker;
    int sv[MAX_SWEEP_WORKERS][2];
    int disk_fd;
    pid_t live_child;
    pthread_t worker_tids[MAX_SWEEP_WORKERS];
    pthread_mutex_t mtx[MAX_SWEEP_WORKERS];
    pthread_cond_t cond[MAX_SWEEP_WORKERS];
    atomic_int arm_state[MAX_SWEEP_WORKERS];
    atomic_uint_fast64_t enter_tsc[MAX_SWEEP_WORKERS];
    atomic_int fallback_fired[MAX_SWEEP_WORKERS];
    atomic_int stop;
    uint64_t rounds[MAX_SWEEP_WORKERS];
    uint64_t armed_ok[MAX_SWEEP_WORKERS];
    uint64_t eintr[MAX_SWEEP_WORKERS];
    uint64_t fallback_eintr[MAX_SWEEP_WORKERS];
    uint64_t ok[MAX_SWEEP_WORKERS];
    uint64_t err[MAX_SWEEP_WORKERS];
    uint64_t w0_rounds[MAX_SWEEP_WORKERS];
    uint64_t w0_eintr[MAX_SWEEP_WORKERS];
    uint64_t wto_rounds[MAX_SWEEP_WORKERS];
    uint64_t wto_eintr[MAX_SWEEP_WORKERS];
    uint64_t woth_rounds[MAX_SWEEP_WORKERS];
    uint64_t woth_eintr[MAX_SWEEP_WORKERS];
    int last_woth[MAX_SWEEP_WORKERS];
    uint64_t buckets[MAX_SWEEP_WORKERS][NUM_LEAD_BUCKETS];
    atomic_uint_fast64_t forks_done;
    atomic_uint_fast64_t fallbacks_sent;
} SweepState;

typedef struct {
    SweepState *st;
    int idx;
} SweepWorkerArg;

static volatile sig_atomic_t g_worker_sig_count[MAX_SWEEP_WORKERS];

static void worker_sig_handler(int sig) {
    if (sig == SIGUSR1) {
        g_worker_sig_count[0]++;
    } else if (sig == SIGUSR2) {
        g_worker_sig_count[1]++;
    }
}

static int worker_signal_for(int idx) {
    return (idx == 0) ? SIGUSR1 : SIGUSR2;
}

static void *sweep_waker_thread(void *arg) {
    SweepState *st = (SweepState *)arg;
    uint64_t wake_ticks = (uint64_t)(EARLY_WAKE_US * g_ticks_per_us);
    for (int loop = 0; loop < MAX_HELPER_LOOPS && !atomic_load(&st->stop); loop++) {
        int did = 0;
        for (int i = 0; i < st->num_workers; i++) {
            if (atomic_load(&st->arm_state[i]) == 1) {
                (void)pthread_mutex_lock(&st->mtx[i]);
                (void)pthread_mutex_unlock(&st->mtx[i]);
                spin_until_tsc(read_tsc() + wake_ticks);
                atomic_store(&st->arm_state[i], 0);
                (void)pthread_cond_signal(&st->cond[i]);
                did = 1;
            }
        }
        if (!did) {
            if (st->enable_pumper) {
                (void)sched_yield();
            } else {
                struct timespec ts = {0, 100000};
                (void)nanosleep(&ts, NULL);
            }
        }
    }
    for (int i = 0; i < st->num_workers; i++) {
        (void)pthread_cond_broadcast(&st->cond[i]);
    }
    return NULL;
}

static void *sweep_pumper_thread(void *arg) {
    SweepState *st = (SweepState *)arg;
    for (int i = 0; i < MAX_PUMPER_YIELDS && !atomic_load(&st->stop); i++) {
        (void)sched_yield();
    }
    return NULL;
}

static void *sweep_unblocker_thread(void *arg) {
    SweepState *st = (SweepState *)arg;
    uint64_t thresh_ticks = (uint64_t)(UNBLOCK_THRESHOLD_MS * 1000.0 * g_ticks_per_us);
    uint64_t last_ent[MAX_SWEEP_WORKERS];
    uint64_t last_send[MAX_SWEEP_WORKERS];
    memset(last_ent, 0, sizeof(last_ent));
    memset(last_send, 0, sizeof(last_send));
    for (int loop = 0; loop < MAX_HELPER_LOOPS && !atomic_load(&st->stop); loop++) {
        uint64_t now = read_tsc();
        for (int i = 0; i < st->num_workers; i++) {
            uint64_t ent = atomic_load(&st->enter_tsc[i]);
            if (ent == 0) {
                last_ent[i] = 0;
                last_send[i] = 0;
                continue;
            }
            if (ent != last_ent[i]) {
                last_ent[i] = ent;
                last_send[i] = 0;
            }
            uint64_t ref = (last_send[i] != 0) ? last_send[i] : ent;
            if (now > ref && (now - ref) >= thresh_ticks) {
                last_send[i] = now;
                atomic_store(&st->fallback_fired[i], 1);
                atomic_fetch_add(&st->fallbacks_sent, 1);
                if (st->target == SWEEP_TARGET_UDS) {
                    char b = 1;
                    (void)write(st->sv[i][1], &b, 1);
                } else if (st->target == SWEEP_TARGET_PROCMGR) {
                    (void)pthread_kill(st->worker_tids[i], worker_signal_for(i));
                }
            }
        }
        struct timespec ts = {0, 2 * NS_PER_MS};
        (void)nanosleep(&ts, NULL);
    }
    return NULL;
}

static void *sweep_forker_thread(void *arg) {
    SweepState *st = (SweepState *)arg;
    for (int i = 0; i < MAX_FORK_CHILDREN && !atomic_load(&st->stop); i++) {
        pid_t pid = fork();
        if (pid == 0) {
            _exit(0);
        }
        if (pid < 0) {
            if (errno == EINTR) {
                continue;
            }
            break;
        }
        int status = 0;
        for (int r = 0; r < 64; r++) {
            pid_t w = waitpid(pid, &status, 0);
            if (w == pid) {
                atomic_fetch_add(&st->forks_done, 1);
                break;
            }
            if (w == -1 && errno == EINTR) {
                continue;
            }
            break;
        }
    }
    return NULL;
}

static void *sweep_worker_thread(void *arg) {
    SweepWorkerArg *wa = (SweepWorkerArg *)arg;
    SweepState *st = wa->st;
    int idx = wa->idx;
    unsigned char block_buf[NVME_BLOCK_BYTES];
    uint64_t arm_ticks = (uint64_t)(ARM_OFFSET_US * g_ticks_per_us);

    for (int round = 0; round < MAX_PART_ROUNDS && !atomic_load(&st->stop); round++) {
        int bucket = round % NUM_LEAD_BUCKETS;
        int lead_us = bucket * SWEEP_STEP_US;
        uint64_t lead_ticks = (uint64_t)(lead_us * g_ticks_per_us);

        (void)pthread_mutex_lock(&st->mtx[idx]);
        int64_t t0_ns = monotonic_ns();
        uint64_t t0_tsc = read_tsc();
        struct timespec abstime = ns_to_timespec(t0_ns + (int64_t)ARM_OFFSET_US * NS_PER_US);
        uint64_t deadline_tsc = t0_tsc + arm_ticks;
        atomic_store(&st->fallback_fired[idx], 0);
        atomic_store(&st->arm_state[idx], 1);
        int wrc = pthread_cond_timedwait(&st->cond[idx], &st->mtx[idx], &abstime);
        (void)pthread_mutex_unlock(&st->mtx[idx]);

        if (atomic_load(&st->stop)) {
            break;
        }

        if (wrc == 0) {
            uint64_t target_tsc = (deadline_tsc > lead_ticks) ? (deadline_tsc - lead_ticks) : 0;
            spin_until_tsc(target_tsc);
        }

        sig_atomic_t sig_before = g_worker_sig_count[idx];
        atomic_store(&st->enter_tsc[idx], read_tsc());
        int is_eintr = 0;
        int is_ok = 0;
        if (st->target == SWEEP_TARGET_UDS) {
            char b = 0;
            ssize_t n = recv(st->sv[idx][0], &b, 1, 0);
            if (n > 0) {
                is_ok = 1;
            } else if (n == -1 && errno == EINTR) {
                is_eintr = 1;
            } else if (n == 0 && atomic_load(&st->stop)) {
                atomic_store(&st->enter_tsc[idx], 0);
                break;
            }
        } else if (st->target == SWEEP_TARGET_NVME) {
            off_t off = (off_t)(round % NVME_LBA_SPAN) * (off_t)NVME_BLOCK_BYTES;
            ssize_t n = pread(st->disk_fd, block_buf, NVME_BLOCK_BYTES, off);
            if (n == NVME_BLOCK_BYTES) {
                is_ok = 1;
            } else if (n == -1 && errno == EINTR) {
                is_eintr = 1;
            }
        } else if (st->target == SWEEP_TARGET_PROCMGR) {
            int status = 0;
            pid_t rc = waitpid(st->live_child, &status, 0);
            if (rc == -1 && errno == EINTR) {
                is_eintr = 1;
            } else if (rc > 0) {
                is_ok = 1;
            }
        }
        atomic_store(&st->enter_tsc[idx], 0);
        int fb = atomic_exchange(&st->fallback_fired[idx], 0);
        int got_sig = (g_worker_sig_count[idx] != sig_before);
        if (atomic_load(&st->stop)) {
            break;
        }

        st->rounds[idx]++;
        if (wrc == 0) {
            st->armed_ok[idx]++;
        }
        st->buckets[idx][bucket]++;
        int is_fb = (fb || got_sig);
        int spont_eintr = (is_eintr && !is_fb);
        if (wrc == 0) {
            st->w0_rounds[idx]++;
            if (spont_eintr) {
                st->w0_eintr[idx]++;
            }
        } else if (wrc == ETIMEDOUT) {
            st->wto_rounds[idx]++;
            if (spont_eintr) {
                st->wto_eintr[idx]++;
            }
        } else {
            st->woth_rounds[idx]++;
            st->last_woth[idx] = wrc;
            if (spont_eintr) {
                st->woth_eintr[idx]++;
            }
        }
        if (is_eintr) {
            if (is_fb) {
                st->fallback_eintr[idx]++;
            } else {
                st->eintr[idx]++;
            }
        } else if (is_ok) {
            st->ok[idx]++;
        } else {
            st->err[idx]++;
        }
    }
    return NULL;
}

static int run_sweep_part(int target, const char *part_tag, const char *disk_path,
                          int num_workers, int enable_pumper, int enable_forker) {
    SweepState st;
    memset(&st, 0, sizeof(st));
    st.target = target;
    st.num_workers = num_workers;
    st.enable_pumper = enable_pumper;
    st.enable_forker = enable_forker;
    st.disk_fd = -1;
    st.live_child = -1;
    for (int i = 0; i < MAX_SWEEP_WORKERS; i++) {
        st.sv[i][0] = -1;
        st.sv[i][1] = -1;
    }
    atomic_init(&st.stop, 0);
    atomic_init(&st.forks_done, 0);
    atomic_init(&st.fallbacks_sent, 0);

    for (int i = 0; i < num_workers; i++) {
        if (pthread_mutex_init(&st.mtx[i], NULL) != 0 || init_monotonic_cond(&st.cond[i]) != 0) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR %s sync init failed\n", part_tag);
            return 2;
        }
        atomic_init(&st.arm_state[i], 0);
        atomic_init(&st.enter_tsc[i], 0);
        atomic_init(&st.fallback_fired[i], 0);
    }

    if (target == SWEEP_TARGET_UDS) {
        for (int i = 0; i < num_workers; i++) {
            if (socketpair(AF_UNIX, SOCK_STREAM, 0, st.sv[i]) != 0) {
                fprintf(stderr, "BLOCKED-WAIT-ERROR %s socketpair failed errno=%d\n", part_tag, errno);
                return 2;
            }
        }
    } else if (target == SWEEP_TARGET_NVME) {
        st.disk_fd = open(disk_path, O_RDONLY | O_CLOEXEC);
        if (st.disk_fd < 0) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR %s open %s failed errno=%d\n", part_tag, disk_path, errno);
            return 2;
        }
    } else if (target == SWEEP_TARGET_PROCMGR) {
        for (int i = 0; i < MAX_SWEEP_WORKERS; i++) {
            g_worker_sig_count[i] = 0;
        }
        struct sigaction sa;
        sa.sa_handler = worker_sig_handler;
        sigemptyset(&sa.sa_mask);
        sa.sa_flags = 0;
        if (sigaction(SIGUSR1, &sa, NULL) != 0 || sigaction(SIGUSR2, &sa, NULL) != 0) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR %s sigaction failed errno=%d\n", part_tag, errno);
            return 2;
        }
        st.live_child = fork();
        if (st.live_child < 0) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR %s fork live child failed errno=%d\n", part_tag, errno);
            return 2;
        }
        if (st.live_child == 0) {
            for (int s = 0; s < PART_RUN_SECS + 30; s++) {
                (void)sleep(1);
            }
            _exit(0);
        }
    }

    int64_t start_ns = monotonic_ns();
    SweepWorkerArg wargs[MAX_SWEEP_WORKERS];
    for (int i = 0; i < num_workers; i++) {
        wargs[i].st = &st;
        wargs[i].idx = i;
        if (pthread_create(&st.worker_tids[i], NULL, sweep_worker_thread, &wargs[i]) != 0) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR %s worker create failed\n", part_tag);
            atomic_store(&st.stop, 1);
            return 2;
        }
    }

    pthread_t waker_tid;
    pthread_t pumper_tid;
    pthread_t unblock_tid;
    pthread_t forker_tid;
    int has_unblocker = (target == SWEEP_TARGET_UDS || target == SWEEP_TARGET_PROCMGR);

    if (pthread_create(&waker_tid, NULL, sweep_waker_thread, &st) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR %s waker create failed\n", part_tag);
        atomic_store(&st.stop, 1);
        return 2;
    }
    if (enable_pumper && pthread_create(&pumper_tid, NULL, sweep_pumper_thread, &st) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR %s pumper create failed\n", part_tag);
        atomic_store(&st.stop, 1);
        return 2;
    }
    if (has_unblocker && pthread_create(&unblock_tid, NULL, sweep_unblocker_thread, &st) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR %s unblocker create failed\n", part_tag);
        atomic_store(&st.stop, 1);
        return 2;
    }
    if (enable_forker && pthread_create(&forker_tid, NULL, sweep_forker_thread, &st) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR %s forker create failed\n", part_tag);
        atomic_store(&st.stop, 1);
        return 2;
    }

    for (int s = 0; s < PART_RUN_SECS; s++) {
        (void)sleep(1);
    }
    atomic_store(&st.stop, 1);

    for (int i = 0; i < num_workers; i++) {
        (void)pthread_cond_broadcast(&st.cond[i]);
    }
    if (target == SWEEP_TARGET_UDS) {
        for (int i = 0; i < num_workers; i++) {
            if (st.sv[i][1] >= 0) {
                (void)close(st.sv[i][1]);
                st.sv[i][1] = -1;
            }
        }
    } else if (target == SWEEP_TARGET_PROCMGR) {
        (void)kill(st.live_child, SIGKILL);
        for (int i = 0; i < num_workers; i++) {
            atomic_store(&st.fallback_fired[i], 1);
            (void)pthread_kill(st.worker_tids[i], worker_signal_for(i));
        }
    }

    (void)pthread_join(waker_tid, NULL);
    if (enable_pumper) {
        (void)pthread_join(pumper_tid, NULL);
    }
    if (has_unblocker) {
        (void)pthread_join(unblock_tid, NULL);
    }
    if (enable_forker) {
        (void)pthread_join(forker_tid, NULL);
    }
    for (int i = 0; i < num_workers; i++) {
        (void)pthread_join(st.worker_tids[i], NULL);
    }

    if (st.live_child > 0) {
        (void)waitpid(st.live_child, NULL, 0);
    }
    for (int i = 0; i < num_workers; i++) {
        if (st.sv[i][0] >= 0) {
            (void)close(st.sv[i][0]);
        }
        if (st.sv[i][1] >= 0) {
            (void)close(st.sv[i][1]);
        }
        (void)pthread_cond_destroy(&st.cond[i]);
        (void)pthread_mutex_destroy(&st.mtx[i]);
    }
    if (st.disk_fd >= 0) {
        (void)close(st.disk_fd);
    }

    uint64_t total_rounds = 0;
    uint64_t total_armed = 0;
    uint64_t total_eintr = 0;
    uint64_t total_fb_eintr = 0;
    uint64_t total_ok = 0;
    uint64_t total_err = 0;
    uint64_t total_w0_r = 0;
    uint64_t total_w0_e = 0;
    uint64_t total_wto_r = 0;
    uint64_t total_wto_e = 0;
    uint64_t total_woth_r = 0;
    uint64_t total_woth_e = 0;
    int last_woth = 0;
    uint64_t merged_buckets[NUM_LEAD_BUCKETS];
    memset(merged_buckets, 0, sizeof(merged_buckets));
    for (int i = 0; i < num_workers; i++) {
        total_rounds += st.rounds[i];
        total_armed += st.armed_ok[i];
        total_eintr += st.eintr[i];
        total_fb_eintr += st.fallback_eintr[i];
        total_ok += st.ok[i];
        total_err += st.err[i];
        total_w0_r += st.w0_rounds[i];
        total_w0_e += st.w0_eintr[i];
        total_wto_r += st.wto_rounds[i];
        total_wto_e += st.wto_eintr[i];
        total_woth_r += st.woth_rounds[i];
        total_woth_e += st.woth_eintr[i];
        if (st.last_woth[i] != 0) {
            last_woth = st.last_woth[i];
        }
        for (int b = 0; b < NUM_LEAD_BUCKETS; b++) {
            merged_buckets[b] += st.buckets[i][b];
        }
    }
    int64_t elapsed_ms = (monotonic_ns() - start_ns) / NS_PER_MS;

    printf("BLOCKED-WAIT-PART%s rounds=%llu armed_ok=%llu eintr=%llu fb_eintr=%llu fallbacks=%llu ok=%llu err=%llu forks=%llu w0=%llu/%llu wto=%llu/%llu woth=%llu/%llu/%d elapsed_ms=%lld\n",
           part_tag,
           (unsigned long long)total_rounds,
           (unsigned long long)total_armed,
           (unsigned long long)total_eintr,
           (unsigned long long)total_fb_eintr,
           (unsigned long long)atomic_load(&st.fallbacks_sent),
           (unsigned long long)total_ok,
           (unsigned long long)total_err,
           (unsigned long long)atomic_load(&st.forks_done),
           (unsigned long long)total_w0_r,
           (unsigned long long)total_w0_e,
           (unsigned long long)total_wto_r,
           (unsigned long long)total_wto_e,
           (unsigned long long)total_woth_r,
           (unsigned long long)total_woth_e,
           last_woth,
           (long long)elapsed_ms);
    printf("BLOCKED-WAIT-LEADS part=%s buckets=", part_tag);
    for (int b = 0; b < NUM_LEAD_BUCKETS; b++) {
        printf("%s%d:%llu", b == 0 ? "" : ",", b * SWEEP_STEP_US, (unsigned long long)merged_buckets[b]);
    }
    printf("\n");
    fflush(stdout);
    return 0;
}

static int run_part_b_control(void) {
    SweepState st;
    memset(&st, 0, sizeof(st));
    atomic_init(&st.stop, 0);
    atomic_init(&st.forks_done, 0);

    pthread_mutex_t mtx;
    pthread_cond_t cond;
    if (pthread_mutex_init(&mtx, NULL) != 0 || init_monotonic_cond(&cond) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR bctrl sync init failed\n");
        return 2;
    }

    pthread_t forker_tid;
    if (pthread_create(&forker_tid, NULL, sweep_forker_thread, &st) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR bctrl forker create failed\n");
        return 2;
    }

    int64_t start_ns = monotonic_ns();
    int timedout = 0;
    int eintr = 0;
    int ok = 0;
    int err = 0;

    for (int r = 0; r < PARTB_CONTROL_ROUNDS; r++) {
        pid_t child = fork();
        if (child < 0) {
            err++;
            continue;
        }
        if (child == 0) {
            struct timespec cs = {0, PARTB_CONTROL_CHILD_MS * NS_PER_MS};
            (void)nanosleep(&cs, NULL);
            _exit(0);
        }

        (void)pthread_mutex_lock(&mtx);
        struct timespec abstime = ns_to_timespec(monotonic_ns() + (int64_t)PARTB_CONTROL_WAIT_MS * NS_PER_MS);
        int wrc = pthread_cond_timedwait(&cond, &mtx, &abstime);
        (void)pthread_mutex_unlock(&mtx);

        int status = 0;
        pid_t w = waitpid(child, &status, 0);
        if (wrc == ETIMEDOUT) {
            timedout++;
        }
        if (w == child) {
            ok++;
        } else if (w == -1 && errno == EINTR) {
            eintr++;
            for (int k = 0; k < 64; k++) {
                if (waitpid(child, &status, 0) == child) {
                    break;
                }
            }
        } else {
            err++;
        }
    }

    atomic_store(&st.stop, 1);
    (void)pthread_join(forker_tid, NULL);
    (void)pthread_cond_destroy(&cond);
    (void)pthread_mutex_destroy(&mtx);

    int64_t elapsed_ms = (monotonic_ns() - start_ns) / NS_PER_MS;
    printf("BLOCKED-WAIT-PARTBCTRL rounds=%d timedout=%d eintr=%d ok=%d err=%d fallbacks=0 forks=%llu elapsed_ms=%lld\n",
           PARTB_CONTROL_ROUNDS, timedout, eintr, ok, err,
           (unsigned long long)atomic_load(&st.forks_done),
           (long long)elapsed_ms);
    fflush(stdout);
    return 0;
}

static int run_in_child(const char *name, int watchdog_s, int (*fn)(const char *), const char *arg) {
    pid_t pid = fork();
    if (pid < 0) {
        printf("BLOCKED-WAIT-CHILD part=%s exit=-1 timed_out=0 fork_errno=%d\n", name, errno);
        fflush(stdout);
        return -1;
    }
    if (pid == 0) {
        _exit(fn(arg));
    }
    int64_t deadline_ns = monotonic_ns() + (int64_t)watchdog_s * NS_PER_SEC;
    int max_polls = watchdog_s * 10 + 20;
    for (int p = 0; p < max_polls; p++) {
        int status = 0;
        pid_t w = waitpid(pid, &status, WNOHANG);
        if (w == pid) {
            int code = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
            printf("BLOCKED-WAIT-CHILD part=%s exit=%d timed_out=0\n", name, code);
            fflush(stdout);
            return code;
        }
        if (w < 0 && errno != EINTR) {
            printf("BLOCKED-WAIT-CHILD part=%s exit=-1 timed_out=0\n", name);
            fflush(stdout);
            return -1;
        }
        if (monotonic_ns() >= deadline_ns) {
            (void)kill(pid, SIGKILL);
            for (int k = 0; k < 20; k++) {
                if (waitpid(pid, &status, WNOHANG) == pid) {
                    break;
                }
                struct timespec ts = {0, 50 * NS_PER_MS};
                (void)nanosleep(&ts, NULL);
            }
            printf("BLOCKED-WAIT-CHILD part=%s exit=-1 timed_out=1\n", name);
            fflush(stdout);
            return 1;
        }
        struct timespec ts = {0, 100 * NS_PER_MS};
        (void)nanosleep(&ts, NULL);
    }
    printf("BLOCKED-WAIT-CHILD part=%s exit=-1 timed_out=1\n", name);
    fflush(stdout);
    return 1;
}

static int part_a_entry(const char *arg) {
    (void)arg;
    return run_part_a();
}

static int part_a2_entry(const char *arg) {
    (void)arg;
    return run_part_a2();
}

static int part_c1_entry(const char *arg) {
    (void)arg;
    return run_sweep_part(SWEEP_TARGET_UDS, "C1", NULL, 2, 1, 0);
}

static int part_c2_entry(const char *arg) {
    return run_sweep_part(SWEEP_TARGET_NVME, "C2", arg, 2, 1, 0);
}

static int part_b_entry(const char *arg) {
    (void)arg;
    return run_sweep_part(SWEEP_TARGET_PROCMGR, "B", NULL, 2, 1, 1);
}

static int part_bctrl_entry(const char *arg) {
    (void)arg;
    return run_part_b_control();
}

static int part_bidle_entry(const char *arg) {
    (void)arg;
    return run_sweep_part(SWEEP_TARGET_PROCMGR, "BIDLE", NULL, 1, 0, 0);
}

static int part_bpump_entry(const char *arg) {
    (void)arg;
    return run_sweep_part(SWEEP_TARGET_PROCMGR, "BPUMP", NULL, 1, 1, 0);
}

static int run_zero_dev(void) {
    struct stat st_zero;
    struct stat st_null;
    if (stat("/scheme/zero", &st_zero) != 0 || stat("/scheme/null", &st_null) != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR stat zero/null failed errno=%d\n", errno);
        return 2;
    }
    printf("BLOCKED-WAIT-ZERO-DEV zero=%llu null=%llu\n",
           (unsigned long long)st_zero.st_dev,
           (unsigned long long)st_null.st_dev);
    fflush(stdout);
    return 0;
}

static int run_zero_read(void) {
    int fd = open("/scheme/zero", O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        printf("BLOCKED-WAIT-ZERO-READ ok=0 bytes=0 errno=%d\n", errno);
        fflush(stdout);
        return 1;
    }
    unsigned char buf[ZERO_CHECK_BYTES];
    memset(buf, 0xaa, sizeof(buf));
    ssize_t n = -1;
    int saved = 0;
    for (int i = 0; i < MAX_PROBE_IO_RETRIES; i++) {
        n = read(fd, buf, sizeof(buf));
        if (n >= 0) {
            break;
        }
        saved = errno;
        if (saved != EINTR) {
            break;
        }
    }
    (void)close(fd);
    int all_zero = (n == ZERO_CHECK_BYTES);
    for (int i = 0; all_zero && i < ZERO_CHECK_BYTES; i++) {
        if (buf[i] != 0) {
            all_zero = 0;
        }
    }
    printf("BLOCKED-WAIT-ZERO-READ ok=%d bytes=%lld errno=%d\n",
           all_zero ? 1 : 0, (long long)(n > 0 ? n : 0), saved);
    fflush(stdout);
    return all_zero ? 0 : 1;
}

static int run_rand_read(void) {
    int fd = open("/scheme/rand", O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        printf("BLOCKED-WAIT-RAND-READ ok=0 bytes=0 errno=%d\n", errno);
        fflush(stdout);
        return 1;
    }
    unsigned char buf[RAND_CHECK_BYTES];
    memset(buf, 0, sizeof(buf));
    ssize_t n = -1;
    int saved = 0;
    for (int i = 0; i < MAX_PROBE_IO_RETRIES; i++) {
        n = read(fd, buf, sizeof(buf));
        if (n >= 0) {
            break;
        }
        saved = errno;
        if (saved != EINTR) {
            break;
        }
    }
    (void)close(fd);
    int any_nonzero = 0;
    for (int i = 0; n == RAND_CHECK_BYTES && i < RAND_CHECK_BYTES; i++) {
        if (buf[i] != 0) {
            any_nonzero = 1;
            break;
        }
    }
    int ok = (n == RAND_CHECK_BYTES && any_nonzero);
    printf("BLOCKED-WAIT-RAND-READ ok=%d bytes=%lld errno=%d\n",
           ok ? 1 : 0, (long long)(n > 0 ? n : 0), saved);
    fflush(stdout);
    return ok ? 0 : 1;
}

static int run_disk_read(const char *disk_path) {
    if (check_unpartitioned_disk(disk_path) != 0) {
        printf("BLOCKED-WAIT-DISK-READ ok=0 bytes=0 errno=0\n");
        fflush(stdout);
        return 2;
    }
    int fd = open(disk_path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) {
        printf("BLOCKED-WAIT-DISK-READ ok=0 bytes=0 errno=%d\n", errno);
        fflush(stdout);
        return 1;
    }
    unsigned char buf[NVME_BLOCK_BYTES];
    ssize_t n = -1;
    int saved = 0;
    for (int i = 0; i < MAX_PROBE_IO_RETRIES; i++) {
        n = pread(fd, buf, sizeof(buf), 0);
        if (n >= 0) {
            break;
        }
        saved = errno;
        if (saved != EINTR) {
            break;
        }
    }
    (void)close(fd);
    int ok = (n == NVME_BLOCK_BYTES);
    printf("BLOCKED-WAIT-DISK-READ ok=%d bytes=%lld errno=%d\n",
           ok ? 1 : 0, (long long)(n > 0 ? n : 0), saved);
    fflush(stdout);
    return ok ? 0 : 1;
}

static int run_uds_ping(void) {
    int sv[2] = {-1, -1};
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        printf("BLOCKED-WAIT-UDS-PING ok=0 errno=%d\n", errno);
        fflush(stdout);
        return 1;
    }
    unsigned char tx = 0x5a;
    unsigned char rx = 0;
    ssize_t wn = -1;
    ssize_t rn = -1;
    int saved = 0;
    for (int i = 0; i < MAX_PROBE_IO_RETRIES; i++) {
        wn = write(sv[1], &tx, 1);
        if (wn >= 0) {
            break;
        }
        saved = errno;
        if (saved != EINTR) {
            break;
        }
    }
    if (wn == 1) {
        for (int i = 0; i < MAX_PROBE_IO_RETRIES; i++) {
            rn = read(sv[0], &rx, 1);
            if (rn >= 0) {
                break;
            }
            saved = errno;
            if (saved != EINTR) {
                break;
            }
        }
    }
    (void)close(sv[0]);
    (void)close(sv[1]);
    int ok = (wn == 1 && rn == 1 && rx == tx);
    printf("BLOCKED-WAIT-UDS-PING ok=%d errno=%d\n", ok ? 1 : 0, saved);
    fflush(stdout);
    return ok ? 0 : 1;
}

static int run_stop_cont(const char *pid_arg) {
    char *end = NULL;
    errno = 0;
    long val = strtol(pid_arg, &end, 10);
    if (errno != 0 || end == pid_arg || *end != '\0' || val <= 1 || val > 2147483647L) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR invalid pid: %s\n", pid_arg);
        return 2;
    }
    pid_t pid = (pid_t)val;
    if (kill(pid, SIGSTOP) != 0) {
        printf("BLOCKED-WAIT-STOP-CONT pid=%ld ok=0 stage=stop errno=%d\n", val, errno);
        fflush(stdout);
        return 1;
    }
    struct timespec rem;
    rem.tv_sec = 0;
    rem.tv_nsec = 200000000L;
    for (int i = 0; i < MAX_PROBE_IO_RETRIES; i++) {
        struct timespec req = rem;
        if (nanosleep(&req, &rem) == 0) {
            break;
        }
        if (errno != EINTR) {
            break;
        }
    }
    if (kill(pid, SIGCONT) != 0) {
        printf("BLOCKED-WAIT-STOP-CONT pid=%ld ok=0 stage=cont errno=%d\n", val, errno);
        fflush(stdout);
        return 1;
    }
    printf("BLOCKED-WAIT-STOP-CONT pid=%ld ok=1\n", val);
    fflush(stdout);
    return 0;
}

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR usage: blocked-wait-probe [--zero-dev|--zero-read|--rand-read|--uds-ping|--disk-read <path>|--stop-cont <pid>|<disk-path>]\n");
        return 2;
    }
    if (strcmp(argv[1], "--zero-dev") == 0) {
        return run_zero_dev();
    }
    if (strcmp(argv[1], "--zero-read") == 0) {
        return run_zero_read();
    }
    if (strcmp(argv[1], "--rand-read") == 0) {
        return run_rand_read();
    }
    if (strcmp(argv[1], "--uds-ping") == 0) {
        return run_uds_ping();
    }
    if (strcmp(argv[1], "--disk-read") == 0) {
        if (argc < 3) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR --disk-read requires /scheme/disk.<pci>-nvme/<unit>\n");
            return 2;
        }
        return run_disk_read(argv[2]);
    }
    if (strcmp(argv[1], "--stop-cont") == 0) {
        if (argc < 3) {
            fprintf(stderr, "BLOCKED-WAIT-ERROR --stop-cont requires <pid>\n");
            return 2;
        }
        return run_stop_cont(argv[2]);
    }
    const char *disk_path = argv[1];
    if (check_unpartitioned_disk(disk_path) != 0) {
        return 2;
    }
    if (calibrate_tsc() != 0) {
        fprintf(stderr, "BLOCKED-WAIT-ERROR TSC calibration failed\n");
        return 2;
    }
    printf("BLOCKED-WAIT-TSC ticks_per_us=%.2f\n", g_ticks_per_us);
    fflush(stdout);

    int rc_a = run_in_child("A", PARTA_WATCHDOG_S, part_a_entry, NULL);
    int rc_a2 = run_in_child("A2", PARTA2_WATCHDOG_S, part_a2_entry, NULL);
    int rc_c1 = run_in_child("C1", PART_WATCHDOG_S, part_c1_entry, NULL);
    int rc_c2 = run_in_child("C2", PART_WATCHDOG_S, part_c2_entry, disk_path);
    int rc_bctrl = run_in_child("BCTRL", PARTB_CONTROL_WATCHDOG_S, part_bctrl_entry, NULL);
    int rc_b = run_in_child("B", PART_WATCHDOG_S, part_b_entry, NULL);
    int rc_bidle = run_in_child("BIDLE", PART_WATCHDOG_S, part_bidle_entry, NULL);
    int rc_bpump = run_in_child("BPUMP", PART_WATCHDOG_S, part_bpump_entry, NULL);

    printf("BLOCKED-WAIT-DONE rc_a=%d rc_a2=%d rc_c1=%d rc_c2=%d rc_bctrl=%d rc_b=%d rc_bidle=%d rc_bpump=%d\n",
           rc_a, rc_a2, rc_c1, rc_c2, rc_bctrl, rc_b, rc_bidle, rc_bpump);
    fflush(stdout);
    return (rc_a == 0 && rc_a2 == 0 && rc_c1 == 0 && rc_c2 == 0 && rc_bctrl == 0 && rc_b == 0 && rc_bidle == 0 && rc_bpump == 0) ? 0 : 1;
}
