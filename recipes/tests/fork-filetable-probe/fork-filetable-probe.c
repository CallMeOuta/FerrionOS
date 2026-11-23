#define _GNU_SOURCE
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define NAME "fork-filetable-probe"
#define CHECKER_PATH "/usr/bin/fork-filetable-checker"
#define TEMP_FILE_PATH "/tmp/fork-filetable-probe.tmp"

#define DEFAULT_ROUNDS 200
#define MAX_ROUNDS 2000
#define MAX_CHURN_ITERS 20000000
#define NUM_CHURN_THREADS 5
#define BATCH_OPEN 40
#define TEMP_FILE_SIZE 4096
#define DUP2_BASE 200
#define DUP2_SLOTS 4
#define FD_CHECK_BOUND 256
#define MAX_SCM_FDS 4
#define MALLOC_RING 16
#define CHILD_WAIT_ITERS 1000
#define CHILD_WAIT_SLEEP_US 5000
#define REAP_WAIT_ITERS 100
#define MAX_DETAIL_LOGS 20
#define PROGRESS_INTERVAL 50

struct child_error {
    int stage;
    int err;
};

static atomic_int g_stop = 0;
static atomic_int g_churn_eexist = 0;
static atomic_int g_churn_ebadf = 0;
static atomic_int g_churn_other = 0;
static atomic_int g_detail_logs = 0;
static atomic_ullong g_fd_churn_ops = 0;
static atomic_ullong g_malloc_churn_ops = 0;

static int g_base_fd = -1;
static int g_sockpair[2] = {-1, -1};

static void record_churn_err(const char *op, int err)
{
    if (err == EEXIST) {
        atomic_fetch_add(&g_churn_eexist, 1);
    } else if (err == EBADF) {
        atomic_fetch_add(&g_churn_ebadf, 1);
    } else {
        atomic_fetch_add(&g_churn_other, 1);
    }
    int logged = atomic_fetch_add(&g_detail_logs, 1);
    if (logged < MAX_DETAIL_LOGS) {
        fprintf(stderr, "%s: churn %s failed errno=%d (%s)\n", NAME, op, err, strerror(err));
    }
}

static uint64_t now_us(void)
{
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000000ULL + (uint64_t)(ts.tv_nsec / 1000L);
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

static void *open_close_dup_worker(void *arg)
{
    (void)arg;
    int fds[BATCH_OPEN];
    for (int iter = 0; iter < MAX_CHURN_ITERS && !atomic_load(&g_stop); iter++) {
        int opened = 0;
        for (int i = 0; i < BATCH_OPEN; i++) {
            int fd = open(TEMP_FILE_PATH, O_RDONLY | O_CLOEXEC);
            if (fd < 0) {
                record_churn_err("open", errno);
                break;
            }
            fds[opened++] = fd;
        }
        int p[2] = {-1, -1};
        if (pipe2(p, O_CLOEXEC) < 0) {
            record_churn_err("pipe2", errno);
        }
        int dup_fd = fcntl(g_base_fd, F_DUPFD_CLOEXEC, 0);
        if (dup_fd < 0) {
            record_churn_err("F_DUPFD_CLOEXEC", errno);
        }
        if (dup_fd >= 0 && close(dup_fd) < 0) {
            record_churn_err("close-dup", errno);
        }
        if (p[0] >= 0 && close(p[0]) < 0) {
            record_churn_err("close-pipe0", errno);
        }
        if (p[1] >= 0 && close(p[1]) < 0) {
            record_churn_err("close-pipe1", errno);
        }
        for (int i = opened - 1; i >= 0; i--) {
            if (close(fds[i]) < 0) {
                record_churn_err("close-open", errno);
            }
        }
        atomic_fetch_add(&g_fd_churn_ops, (unsigned long long)(opened * 2 + 4));
    }
    return NULL;
}

static void *dup2_worker(void *arg)
{
    (void)arg;
    for (int iter = 0; iter < MAX_CHURN_ITERS && !atomic_load(&g_stop); iter++) {
        int target = DUP2_BASE + (iter % DUP2_SLOTS);
        if (dup2(g_base_fd, target) < 0) {
            record_churn_err("dup2", errno);
        } else if (close(target) < 0) {
            record_churn_err("close-dup2", errno);
        }
        int nfd = open("/dev/null", O_RDONLY | O_CLOEXEC);
        if (nfd < 0) {
            record_churn_err("open-null", errno);
        } else if (close(nfd) < 0) {
            record_churn_err("close-null", errno);
        }
        void *mapped = mmap(NULL, TEMP_FILE_SIZE, PROT_READ, MAP_SHARED, g_base_fd, 0);
        if (mapped == MAP_FAILED) {
            record_churn_err("mmap-shared", errno);
        } else if (munmap(mapped, TEMP_FILE_SIZE) < 0) {
            record_churn_err("munmap-shared", errno);
        }
        atomic_fetch_add(&g_fd_churn_ops, 6ULL);
    }
    return NULL;
}

static int send_scm_fds(int sock, const int *fds, size_t nfds)
{
    assert(nfds > 0 && nfds <= MAX_SCM_FDS);
    char payload[1] = {'X'};
    char control[CMSG_SPACE(sizeof(int) * MAX_SCM_FDS)];
    struct iovec iov = {payload, sizeof(payload)};
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    memset(control, 0, sizeof(control));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = CMSG_LEN(sizeof(int) * nfds);
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    assert(cmsg != NULL);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = msg.msg_controllen;
    memcpy(CMSG_DATA(cmsg), fds, sizeof(int) * nfds);
    ssize_t n = sendmsg(sock, &msg, MSG_DONTWAIT);
    return (n == 1) ? 0 : -1;
}

static int recv_scm_fds(int sock, int *out_fds, size_t *out_nfds)
{
    char payload[1] = {0};
    char control[CMSG_SPACE(sizeof(int) * MAX_SCM_FDS) + 32];
    struct iovec iov = {payload, sizeof(payload)};
    struct msghdr msg;
    memset(&msg, 0, sizeof(msg));
    memset(control, 0, sizeof(control));
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = sizeof(control);
    *out_nfds = 0;
    ssize_t n = recvmsg(sock, &msg, MSG_DONTWAIT | MSG_CMSG_CLOEXEC);
    if (n != 1) {
        return -1;
    }
    if (msg.msg_flags & MSG_CTRUNC) {
        errno = EMSGSIZE;
        return -1;
    }
    for (struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg); cmsg != NULL; cmsg = CMSG_NXTHDR(&msg, cmsg)) {
        if (cmsg->cmsg_level == SOL_SOCKET && cmsg->cmsg_type == SCM_RIGHTS) {
            size_t count = (cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int);
            assert(*out_nfds + count <= MAX_SCM_FDS);
            memcpy(out_fds + *out_nfds, CMSG_DATA(cmsg), count * sizeof(int));
            *out_nfds += count;
        }
    }
    return 0;
}

static void *scm_rights_worker(void *arg)
{
    (void)arg;
    for (int iter = 0; iter < MAX_CHURN_ITERS && !atomic_load(&g_stop); iter++) {
        int tmp = open(TEMP_FILE_PATH, O_RDONLY | O_CLOEXEC);
        if (tmp < 0) {
            record_churn_err("scm-open", errno);
            continue;
        }
        int pair[2] = {g_base_fd, tmp};
        if (send_scm_fds(g_sockpair[0], pair, 2) < 0) {
            if (errno != EAGAIN && errno != EWOULDBLOCK) {
                record_churn_err("scm-sendmsg", errno);
            }
            if (close(tmp) < 0) {
                record_churn_err("scm-close-tmp-err", errno);
            }
            continue;
        }
        int rfds[MAX_SCM_FDS];
        size_t rnfds = 0;
        int got = 0;
        for (int tries = 0; tries < 200; tries++) {
            if (recv_scm_fds(g_sockpair[1], rfds, &rnfds) == 0) {
                got = 1;
                break;
            }
            if (errno != EAGAIN && errno != EWOULDBLOCK) {
                record_churn_err("scm-recvmsg", errno);
                break;
            }
            usleep(100);
        }
        if (got) {
            if (rnfds != 2) {
                record_churn_err("scm-count", EPROTO);
            }
            for (size_t i = 0; i < rnfds; i++) {
                if (close(rfds[i]) < 0) {
                    record_churn_err("scm-close-recv", errno);
                }
            }
        }
        if (close(tmp) < 0) {
            record_churn_err("scm-close-tmp", errno);
        }
        atomic_fetch_add(&g_fd_churn_ops, 6ULL);
    }
    return NULL;
}

static void *malloc_churn_worker(void *arg)
{
    (void)arg;
    static const size_t sizes[5] = {32, 128, 512, 2048, 4096};
    void *ring[MALLOC_RING];
    memset(ring, 0, sizeof(ring));
    for (int iter = 0; iter < MAX_CHURN_ITERS && !atomic_load(&g_stop); iter++) {
        size_t slot = (size_t)(iter % MALLOC_RING);
        size_t sz = sizes[(size_t)(iter % 5)];
        if (ring[slot] != NULL) {
            free(ring[slot]);
            ring[slot] = NULL;
        }
        void *p = malloc(sz);
        if (p == NULL) {
            record_churn_err("malloc", ENOMEM);
            continue;
        }
        memset(p, (int)(iter & 0xff), sz);
        ring[slot] = p;
        atomic_fetch_add(&g_malloc_churn_ops, 2ULL);
    }
    for (int i = 0; i < MALLOC_RING; i++) {
        if (ring[i] != NULL) {
            free(ring[i]);
        }
    }
    return NULL;
}

static void set_fd_cloexec_nonblock(int fd)
{
    int fdflags = fcntl(fd, F_GETFD);
    assert(fdflags >= 0);
    assert(fcntl(fd, F_SETFD, fdflags | FD_CLOEXEC) == 0);
    int flflags = fcntl(fd, F_GETFL);
    assert(flflags >= 0);
    assert(fcntl(fd, F_SETFL, flflags | O_NONBLOCK) == 0);
}

extern char **environ;

static int run_spawn_rounds(int rounds)
{
    uint64_t *round_us = calloc((size_t)rounds, sizeof(uint64_t));
    assert(round_us != NULL);

    uint64_t loop_t0 = now_us();
    unsigned long long fd_ops_start = atomic_load(&g_fd_churn_ops);
    unsigned long long malloc_ops_start = atomic_load(&g_malloc_churn_ops);

    int completed = 0;
    int spawn_err = 0;
    int stuck = 0;
    int leaked = 0;
    int other_exit = 0;

    char *const child_argv[] = {(char *)CHECKER_PATH, NULL};

    for (int r = 0; r < rounds; r++) {
        uint64_t t0 = now_us();
        pid_t pid = -1;
        int rc = posix_spawn(&pid, CHECKER_PATH, NULL, NULL, child_argv, environ);
        if (rc != 0) {
            spawn_err++;
            int logged = atomic_fetch_add(&g_detail_logs, 1);
            if (logged < MAX_DETAIL_LOGS) {
                fprintf(stderr, "%s: spawn round %d failed rc=%d (%s)\n", NAME, r, rc, strerror(rc));
            }
            continue;
        }
        int status = 0;
        pid_t w = 0;
        for (int i = 0; i < CHILD_WAIT_ITERS; i++) {
            w = waitpid(pid, &status, WNOHANG);
            if (w == pid || (w < 0 && errno != EINTR)) {
                break;
            }
            usleep(CHILD_WAIT_SLEEP_US);
        }
        if (w != pid) {
            stuck++;
            int logged = atomic_fetch_add(&g_detail_logs, 1);
            if (logged < MAX_DETAIL_LOGS) {
                fprintf(stderr, "%s: spawn round %d child pid=%d stuck\n", NAME, r, (int)pid);
            }
            (void)kill(pid, SIGKILL);
            for (int i = 0; i < REAP_WAIT_ITERS; i++) {
                pid_t rw = waitpid(pid, &status, WNOHANG);
                if (rw == pid || (rw < 0 && errno != EINTR)) {
                    break;
                }
                usleep(CHILD_WAIT_SLEEP_US);
            }
            continue;
        }
        round_us[completed++] = now_us() - t0;
        if (WIFEXITED(status)) {
            int code = WEXITSTATUS(status);
            if (code == 0) {
            } else if (code == 42) {
                leaked++;
            } else {
                other_exit++;
                int logged = atomic_fetch_add(&g_detail_logs, 1);
                if (logged < MAX_DETAIL_LOGS) {
                    fprintf(stderr, "%s: spawn round %d child exit=%d\n", NAME, r, code);
                }
            }
        } else {
            other_exit++;
            int logged = atomic_fetch_add(&g_detail_logs, 1);
            if (logged < MAX_DETAIL_LOGS) {
                fprintf(stderr, "%s: spawn round %d child status=0x%x\n", NAME, r, status);
            }
        }
        if ((r + 1) % PROGRESS_INTERVAL == 0 || r + 1 == rounds) {
            printf("%s: spawn progress %d/%d spawn_err=%d stuck=%d leaked=%d churn_eexist=%d churn_ebadf=%d\n",
                   NAME,
                   r + 1,
                   rounds,
                   spawn_err,
                   stuck,
                   leaked,
                   atomic_load(&g_churn_eexist),
                   atomic_load(&g_churn_ebadf));
            fflush(stdout);
        }
    }

    uint64_t loop_dt_us = now_us() - loop_t0;
    unsigned long long fd_ops = atomic_load(&g_fd_churn_ops) - fd_ops_start;
    unsigned long long malloc_ops = atomic_load(&g_malloc_churn_ops) - malloc_ops_start;
    unsigned long long fd_ops_s = loop_dt_us > 0 ? (fd_ops * 1000000ULL) / loop_dt_us : 0ULL;
    unsigned long long malloc_ops_s = loop_dt_us > 0 ? (malloc_ops * 1000000ULL) / loop_dt_us : 0ULL;

    uint64_t median_us = 0;
    uint64_t p95_us = 0;
    if (completed > 0) {
        qsort(round_us, (size_t)completed, sizeof(uint64_t), cmp_u64);
        median_us = round_us[completed / 2];
        size_t p95_idx = (size_t)((completed - 1) * 95 / 100);
        p95_us = round_us[p95_idx];
    }
    free(round_us);

    int failures = spawn_err + stuck + leaked + other_exit;
    printf("%s: spawn summary rounds=%d completed=%d median_us=%llu p95_us=%llu fd_ops_s=%llu malloc_ops_s=%llu spawn_err=%d stuck=%d leaked=%d other_exit=%d\n",
           NAME,
           rounds,
           completed,
           (unsigned long long)median_us,
           (unsigned long long)p95_us,
           fd_ops_s,
           malloc_ops_s,
           spawn_err,
           stuck,
           leaked,
           other_exit);
    fflush(stdout);
    return failures;
}

int main(int argc, char **argv)
{
    int rounds = DEFAULT_ROUNDS;
    int run_fork = 1;
    int run_spawn = 1;
    int argi = 1;
    if (argi < argc && strcmp(argv[argi], "--fork") == 0) {
        run_spawn = 0;
        argi++;
    } else if (argi < argc && strcmp(argv[argi], "--spawn") == 0) {
        run_fork = 0;
        argi++;
    }
    if (argi < argc) {
        long parsed = strtol(argv[argi], NULL, 10);
        if (parsed < 1 || parsed > MAX_ROUNDS) {
            fprintf(stderr, "%s: FAIL invalid rounds %ld (1..%d)\n", NAME, parsed, MAX_ROUNDS);
            return 1;
        }
        rounds = (int)parsed;
    }

    int inherited_closed = 0;
    for (int fd = 3; fd < FD_CHECK_BOUND; fd++) {
        if (close(fd) == 0) {
            inherited_closed++;
        }
    }
    printf("%s: start rounds=%d inherited_closed=%d\n", NAME, rounds, inherited_closed);
    fflush(stdout);

    int init_fd = open(TEMP_FILE_PATH, O_CREAT | O_RDWR | O_TRUNC | O_CLOEXEC, 0600);
    if (init_fd < 0) {
        fprintf(stderr, "%s: FAIL create temp file errno=%d\n", NAME, errno);
        return 1;
    }
    char page_buf[TEMP_FILE_SIZE];
    memset(page_buf, 'A', sizeof(page_buf));
    if (write(init_fd, page_buf, sizeof(page_buf)) != (ssize_t)sizeof(page_buf)) {
        fprintf(stderr, "%s: FAIL write temp file errno=%d\n", NAME, errno);
        close(init_fd);
        return 1;
    }
    assert(close(init_fd) == 0);

    g_base_fd = open(TEMP_FILE_PATH, O_RDONLY | O_CLOEXEC);
    if (g_base_fd < 0) {
        fprintf(stderr, "%s: FAIL open base_fd errno=%d\n", NAME, errno);
        return 1;
    }

    if (socketpair(AF_UNIX, SOCK_STREAM, 0, g_sockpair) < 0) {
        fprintf(stderr, "%s: FAIL socketpair errno=%d\n", NAME, errno);
        return 1;
    }
    set_fd_cloexec_nonblock(g_sockpair[0]);
    set_fd_cloexec_nonblock(g_sockpair[1]);

    pthread_t threads[NUM_CHURN_THREADS];
    assert(pthread_create(&threads[0], NULL, open_close_dup_worker, NULL) == 0);
    assert(pthread_create(&threads[1], NULL, open_close_dup_worker, NULL) == 0);
    assert(pthread_create(&threads[2], NULL, dup2_worker, NULL) == 0);
    assert(pthread_create(&threads[3], NULL, scm_rights_worker, NULL) == 0);
    assert(pthread_create(&threads[4], NULL, malloc_churn_worker, NULL) == 0);

    usleep(20000);

    int fork_failures = 0;
    int spawn_failures = 0;

    if (run_fork) {
        uint64_t *round_us = calloc((size_t)rounds, sizeof(uint64_t));
        assert(round_us != NULL);

        uint64_t loop_t0 = now_us();
        unsigned long long fd_ops_start = atomic_load(&g_fd_churn_ops);
        unsigned long long malloc_ops_start = atomic_load(&g_malloc_churn_ops);

        int completed = 0;
        int fork_err = 0;
        int fork_eexist = 0;
        int child_open_err = 0;
        int child_open_eexist = 0;
        int exec_err = 0;
        int exec_eexist = 0;
        int stuck = 0;
        int leaked = 0;
        int other_exit = 0;

        for (int r = 0; r < rounds; r++) {
            int errpipe[2] = {-1, -1};
            if (pipe2(errpipe, O_CLOEXEC) < 0) {
                record_churn_err("errpipe", errno);
                continue;
            }
            uint64_t t0 = now_us();
            pid_t pid = fork();
            if (pid < 0) {
                int err = errno;
                fork_err++;
                if (err == EEXIST) {
                    fork_eexist++;
                }
                int logged = atomic_fetch_add(&g_detail_logs, 1);
                if (logged < MAX_DETAIL_LOGS) {
                    fprintf(stderr, "%s: round %d fork failed errno=%d (%s)\n", NAME, r, err, strerror(err));
                }
                close(errpipe[0]);
                close(errpipe[1]);
                continue;
            }
            if (pid == 0) {
                close(errpipe[0]);
                int p1 = open("/dev/null", O_RDONLY | O_CLOEXEC);
                if (p1 < 0) {
                    struct child_error ce = {1, errno};
                    (void)write(errpipe[1], &ce, sizeof(ce));
                    _exit(126);
                }
                int p2 = open(TEMP_FILE_PATH, O_RDONLY | O_CLOEXEC);
                if (p2 < 0) {
                    struct child_error ce = {1, errno};
                    (void)write(errpipe[1], &ce, sizeof(ce));
                    _exit(126);
                }
                if (close(p2) < 0 || close(p1) < 0) {
                    struct child_error ce = {1, errno};
                    (void)write(errpipe[1], &ce, sizeof(ce));
                    _exit(126);
                }
                char *const child_argv[] = {(char *)CHECKER_PATH, NULL};
                execv(CHECKER_PATH, child_argv);
                struct child_error ce = {2, errno};
                (void)write(errpipe[1], &ce, sizeof(ce));
                _exit(127);
            }

            if (close(errpipe[1]) < 0) {
                record_churn_err("close-errpipe1", errno);
            }
            int status = 0;
            pid_t w = 0;
            for (int i = 0; i < CHILD_WAIT_ITERS; i++) {
                w = waitpid(pid, &status, WNOHANG);
                if (w == pid || (w < 0 && errno != EINTR)) {
                    break;
                }
                usleep(CHILD_WAIT_SLEEP_US);
            }
            if (w != pid) {
                stuck++;
                int logged = atomic_fetch_add(&g_detail_logs, 1);
                if (logged < MAX_DETAIL_LOGS) {
                    fprintf(stderr, "%s: round %d child pid=%d stuck\n", NAME, r, (int)pid);
                }
                (void)kill(pid, SIGKILL);
                for (int i = 0; i < REAP_WAIT_ITERS; i++) {
                    pid_t rw = waitpid(pid, &status, WNOHANG);
                    if (rw == pid || (rw < 0 && errno != EINTR)) {
                        break;
                    }
                    usleep(CHILD_WAIT_SLEEP_US);
                }
                close(errpipe[0]);
                continue;
            }

            uint64_t dt = now_us() - t0;
            round_us[completed++] = dt;

            struct child_error ce = {0, 0};
            ssize_t nread = read(errpipe[0], &ce, sizeof(ce));
            if (close(errpipe[0]) < 0) {
                record_churn_err("close-errpipe0", errno);
            }

            if (WIFEXITED(status)) {
                int code = WEXITSTATUS(status);
                if (code == 0) {
                } else if (code == 42) {
                    leaked++;
                } else if (code == 126) {
                    child_open_err++;
                    if (nread == (ssize_t)sizeof(ce) && ce.err == EEXIST) {
                        child_open_eexist++;
                    }
                    int logged = atomic_fetch_add(&g_detail_logs, 1);
                    if (logged < MAX_DETAIL_LOGS) {
                        fprintf(stderr, "%s: round %d child open/close failed errno=%d (%s)\n", NAME, r, ce.err, strerror(ce.err));
                    }
                } else if (code == 127) {
                    exec_err++;
                    if (nread == (ssize_t)sizeof(ce) && ce.err == EEXIST) {
                        exec_eexist++;
                    }
                    int logged = atomic_fetch_add(&g_detail_logs, 1);
                    if (logged < MAX_DETAIL_LOGS) {
                        fprintf(stderr, "%s: round %d execv failed errno=%d (%s)\n", NAME, r, ce.err, strerror(ce.err));
                    }
                } else {
                    other_exit++;
                    int logged = atomic_fetch_add(&g_detail_logs, 1);
                    if (logged < MAX_DETAIL_LOGS) {
                        fprintf(stderr, "%s: round %d child exit=%d\n", NAME, r, code);
                    }
                }
            } else {
                other_exit++;
                int logged = atomic_fetch_add(&g_detail_logs, 1);
                if (logged < MAX_DETAIL_LOGS) {
                    fprintf(stderr, "%s: round %d child status=0x%x\n", NAME, r, status);
                }
            }

            if ((r + 1) % PROGRESS_INTERVAL == 0 || r + 1 == rounds) {
                printf("%s: progress %d/%d fork_err=%d child_open_err=%d exec_err=%d stuck=%d leaked=%d churn_eexist=%d churn_ebadf=%d\n",
                       NAME,
                       r + 1,
                       rounds,
                       fork_err,
                       child_open_err,
                       exec_err,
                       stuck,
                       leaked,
                       atomic_load(&g_churn_eexist),
                       atomic_load(&g_churn_ebadf));
                fflush(stdout);
            }
        }

        uint64_t loop_dt_us = now_us() - loop_t0;
        unsigned long long fd_ops = atomic_load(&g_fd_churn_ops) - fd_ops_start;
        unsigned long long malloc_ops = atomic_load(&g_malloc_churn_ops) - malloc_ops_start;
        unsigned long long fd_ops_s = loop_dt_us > 0 ? (fd_ops * 1000000ULL) / loop_dt_us : 0ULL;
        unsigned long long malloc_ops_s = loop_dt_us > 0 ? (malloc_ops * 1000000ULL) / loop_dt_us : 0ULL;

        uint64_t median_us = 0;
        uint64_t p95_us = 0;
        if (completed > 0) {
            qsort(round_us, (size_t)completed, sizeof(uint64_t), cmp_u64);
            median_us = round_us[completed / 2];
            size_t p95_idx = (size_t)((completed - 1) * 95 / 100);
            p95_us = round_us[p95_idx];
        }
        free(round_us);

        fork_failures = fork_err + child_open_err + exec_err + stuck + leaked + other_exit;

        printf("%s: summary rounds=%d completed=%d median_us=%llu p95_us=%llu fd_ops_s=%llu malloc_ops_s=%llu fork_err=%d fork_eexist=%d child_open_err=%d child_open_eexist=%d exec_err=%d exec_eexist=%d stuck=%d leaked=%d other_exit=%d\n",
               NAME,
               rounds,
               completed,
               (unsigned long long)median_us,
               (unsigned long long)p95_us,
               fd_ops_s,
               malloc_ops_s,
               fork_err,
               fork_eexist,
               child_open_err,
               child_open_eexist,
               exec_err,
               exec_eexist,
               stuck,
               leaked,
               other_exit);
        fflush(stdout);
    }

    if (run_spawn) {
        spawn_failures = run_spawn_rounds(rounds);
    }

    atomic_store(&g_stop, 1);
    for (int i = 0; i < NUM_CHURN_THREADS; i++) {
        assert(pthread_join(threads[i], NULL) == 0);
    }

    close(g_sockpair[0]);
    close(g_sockpair[1]);
    close(g_base_fd);
    unlink(TEMP_FILE_PATH);

    int ceexist = atomic_load(&g_churn_eexist);
    int cebadf = atomic_load(&g_churn_ebadf);
    int cother = atomic_load(&g_churn_other);
    int churn_failures = ceexist + cebadf + cother;
    int total_failures = fork_failures + spawn_failures + churn_failures;

    printf("%s: churn summary churn_eexist=%d churn_ebadf=%d churn_other=%d\n", NAME, ceexist, cebadf, cother);
    if (run_fork && fork_failures == 0 && churn_failures == 0) {
        printf("%s: pass fork-exec-churn\n", NAME);
    }
    if (run_spawn && spawn_failures == 0 && churn_failures == 0) {
        printf("%s: pass spawn-churn\n", NAME);
    }
    fflush(stdout);

    if (total_failures == 0) {
        printf("%s: PASS\n", NAME);
        return 0;
    }
    printf("%s: FAIL total_failures=%d\n", NAME, total_failures);
    return 1;
}
