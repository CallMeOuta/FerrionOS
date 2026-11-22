#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/timerfd.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void fail(const char *expression, int line) {
    fprintf(stderr, "fd-primitives-probe: FAIL line %d: %s (errno %d: %s)\n",
            line, expression, errno, strerror(errno));
    exit(1);
}

#define CHECK(expression) do { if (!(expression)) fail(#expression, __LINE__); } while (0)
#define ERROR(expression, expected) do { \
    errno = 0; \
    CHECK((expression) == -1 && errno == (expected)); \
} while (0)

static void timed_out(int signal_number) {
    static const char message[] = "fd-primitives-probe: FAIL alarm timeout\n";
    (void)signal_number;
    (void)write(STDERR_FILENO, message, sizeof message - 1);
    _exit(124);
}

static void pass(const char *name) {
    printf("fd-primitives-probe: pass %s\n", name);
}

static uint64_t read_count(int fd) {
    uint64_t value = 0;
    CHECK(read(fd, &value, sizeof value) == (ssize_t)sizeof value);
    return value;
}

static void write_count(int fd, uint64_t value) {
    CHECK(write(fd, &value, sizeof value) == (ssize_t)sizeof value);
}

static void empty(int fd) {
    uint64_t value;
    ERROR(read(fd, &value, sizeof value), EAGAIN);
}

static void child_ok(pid_t child) {
    int status = 0;
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

static int watch(int fd, uint32_t events) {
    int ep = epoll_create1(EPOLL_CLOEXEC);
    CHECK(ep >= 0);
    struct epoll_event event = {0};
    event.events = events;
    event.data.fd = fd;
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, fd, &event) == 0);
    return ep;
}

static void poll_events(int fd, short expected, int timeout) {
    struct pollfd item = { .fd = fd, .events = POLLIN | POLLOUT };
    CHECK(poll(&item, 1, timeout) == (expected != 0));
    CHECK(item.revents == expected);
}

static void epoll_events(int ep, int fd, uint32_t expected, int timeout) {
    struct epoll_event event = {0};
    CHECK(epoll_wait(ep, &event, 1, timeout) == (expected != 0));
    if (expected) {
        CHECK(event.data.fd == fd);
        CHECK(event.events == expected);
    }
}

static void eventfd_counter(void) {
    ERROR(eventfd(0, -1), EINVAL);
    int fd = eventfd(2, EFD_NONBLOCK);
    CHECK(fd >= 0);
    write_count(fd, 3);
    write_count(fd, 4);
    CHECK(read_count(fd) == 9);
    empty(fd);
    write_count(fd, 0);
    empty(fd);

    uint64_t value = 7;
    ERROR(write(fd, &value, sizeof value - 1), EINVAL);
    empty(fd);
    write_count(fd, value);
    ERROR(read(fd, &value, sizeof value - 1), EINVAL);
    CHECK(read_count(fd) == 7);
    value = UINT64_MAX;
    ERROR(write(fd, &value, sizeof value), EINVAL);
    empty(fd);
    write_count(fd, UINT64_MAX - 1);
    value = 1;
    ERROR(write(fd, &value, sizeof value), EAGAIN);
    CHECK(read_count(fd) == UINT64_MAX - 1);
    empty(fd);
    uint64_t buffer[2] = {0, UINT64_MAX};
    write_count(fd, 11);
    CHECK(read(fd, buffer, sizeof buffer) == (ssize_t)sizeof buffer[0]);
    CHECK(buffer[0] == 11);
    ERROR(write(fd, buffer, sizeof buffer), EINVAL);
    empty(fd);
    CHECK(close(fd) == 0);
    pass("eventfd-counter-errors");
}

static void eventfd_semaphore(void) {
    int fd = eventfd(2, EFD_SEMAPHORE | EFD_NONBLOCK);
    CHECK(fd >= 0);
    CHECK(read_count(fd) == 1);
    CHECK(read_count(fd) == 1);
    empty(fd);
    write_count(fd, 3);
    CHECK(read_count(fd) == 1);
    CHECK(read_count(fd) == 1);
    CHECK(read_count(fd) == 1);
    empty(fd);
    CHECK(close(fd) == 0);
    pass("eventfd-semaphore");
}

static void eventfd_readiness(void) {
    int fd = eventfd(0, EFD_NONBLOCK);
    CHECK(fd >= 0);
    poll_events(fd, POLLOUT, 0);
    write_count(fd, 5);
    poll_events(fd, POLLIN | POLLOUT, 0);
    CHECK(read_count(fd) == 5);
    poll_events(fd, POLLOUT, 0);
    write_count(fd, UINT64_MAX - 1);
    poll_events(fd, POLLIN, 0);
    CHECK(read_count(fd) == UINT64_MAX - 1);
    poll_events(fd, POLLOUT, 0);
    int ep = watch(fd, EPOLLIN);
    epoll_events(ep, fd, 0, 0);
    write_count(fd, 5);
    epoll_events(ep, fd, EPOLLIN, 2000);
    CHECK(close(ep) == 0);
    poll_events(fd, POLLIN | POLLOUT, 0);
    CHECK(read_count(fd) == 5);
    poll_events(fd, POLLOUT, 0);
    CHECK(close(fd) == 0);
    pass("eventfd-poll-epoll");
}

static void eventfd_shared(void) {
    int fd = eventfd(4, 0);
    CHECK(fd >= 0);
    int alias = dup(fd);
    CHECK(alias >= 0);
    CHECK(read_count(alias) == 4);
    write_count(alias, 6);
    CHECK(read_count(fd) == 6);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        alarm(10);
        CHECK(read_count(alias) == 13);
        write_count(alias, 17);
        _exit(0);
    }
    write_count(fd, 13);
    child_ok(child);
    CHECK(read_count(fd) == 17);
    CHECK(close(fd) == 0);
    write_count(alias, 19);
    CHECK(read_count(alias) == 19);
    CHECK(close(alias) == 0);
    pass("eventfd-dup-fork-blocking");
}

static struct itimerspec timer_value(time_t seconds, long nanoseconds, long interval) {
    struct itimerspec value = {0};
    value.it_value.tv_sec = seconds;
    value.it_value.tv_nsec = nanoseconds;
    value.it_interval.tv_nsec = interval;
    return value;
}

static int is_zero(struct timespec value) {
    return value.tv_sec == 0 && value.tv_nsec == 0;
}

static void timerfd_errors(void) {
    ERROR(timerfd_create((clockid_t)-1, 0), EINVAL);
    ERROR(timerfd_create(CLOCK_MONOTONIC, -1), EINVAL);
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    CHECK(fd >= 0);
    struct itimerspec value = timer_value(0, 0, 0);
    struct itimerspec current;
    CHECK(timerfd_gettime(fd, &current) == 0);
    CHECK(is_zero(current.it_value) && is_zero(current.it_interval));
    empty(fd);
    ERROR(timerfd_settime(-1, 0, &value, NULL), EBADF);
    ERROR(timerfd_gettime(-1, &current), EBADF);
    ERROR(timerfd_settime(fd, -1, &value, NULL), EINVAL);
    value.it_value.tv_nsec = 1000000000L;
    ERROR(timerfd_settime(fd, 0, &value, NULL), EINVAL);
    value.it_value.tv_nsec = -1;
    ERROR(timerfd_settime(fd, 0, &value, NULL), EINVAL);
    value = timer_value(-1, 0, 0);
    ERROR(timerfd_settime(fd, 0, &value, NULL), EINVAL);
    value = timer_value(0, 0, 1000000000L);
    ERROR(timerfd_settime(fd, 0, &value, NULL), EINVAL);
    value.it_interval.tv_nsec = -1;
    ERROR(timerfd_settime(fd, 0, &value, NULL), EINVAL);
    value = timer_value(0, 0, 0);
    value.it_interval.tv_sec = -1;
    ERROR(timerfd_settime(fd, 0, &value, NULL), EINVAL);
    CHECK(timerfd_gettime(fd, &current) == 0);
    CHECK(is_zero(current.it_value) && is_zero(current.it_interval));
    CHECK(close(fd) == 0);
    fd = timerfd_create(CLOCK_REALTIME, TFD_NONBLOCK);
    CHECK(fd >= 0);
    value = timer_value(0, 0, 0);
    CHECK(clock_gettime(CLOCK_REALTIME, &value.it_value) == 0);
    value.it_value.tv_sec += 60;
#ifdef __redox__
    ERROR(timerfd_settime(fd, TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET, &value, NULL), EINVAL);
    CHECK(timerfd_gettime(fd, &current) == 0);
    CHECK(is_zero(current.it_value) && is_zero(current.it_interval));
#else
    CHECK(timerfd_settime(fd, TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET, &value, NULL) == 0);
#endif
    CHECK(close(fd) == 0);
    pass("timerfd-errors");
}

static void timerfd_clock(clockid_t clock, const char *marker) {
    int fd = timerfd_create(clock, TFD_NONBLOCK);
    CHECK(fd >= 0);
    poll_events(fd, 0, 0);
    for (int absolute = 0; absolute <= 1; absolute++) {
        for (int use_epoll = 0; use_epoll <= 1; use_epoll++) {
            int ep = -1;
            if (use_epoll) {
                ep = watch(fd, EPOLLIN);
                epoll_events(ep, fd, 0, 0);
            }
            struct itimerspec value = timer_value(0, 20000000L, 0);
            if (absolute) {
                CHECK(clock_gettime(clock, &value.it_value) == 0);
                value.it_value.tv_nsec += 20000000L;
                if (value.it_value.tv_nsec >= 1000000000L) {
                    value.it_value.tv_sec++;
                    value.it_value.tv_nsec -= 1000000000L;
                }
            }
            CHECK(timerfd_settime(fd, absolute ? TFD_TIMER_ABSTIME : 0, &value, NULL) == 0);
            if (use_epoll) {
                epoll_events(ep, fd, EPOLLIN, 2000);
                CHECK(close(ep) == 0);
                poll_events(fd, POLLIN, 0);
            } else {
                poll_events(fd, POLLIN, 2000);
            }
            uint64_t count = 0;
            ERROR(read(fd, &count, sizeof count - 1), EINVAL);
            CHECK(read_count(fd) == 1);
            empty(fd);
            poll_events(fd, 0, 0);
            struct itimerspec current;
            CHECK(timerfd_gettime(fd, &current) == 0);
            CHECK(is_zero(current.it_value) && is_zero(current.it_interval));
        }
    }
    CHECK(close(fd) == 0);
    pass(marker);
}

static void timerfd_rearm(void) {
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    CHECK(fd >= 0);
    struct itimerspec value = timer_value(60, 0, 20000000L);
    struct itimerspec old;
    CHECK(timerfd_settime(fd, 0, &value, &old) == 0);
    CHECK(is_zero(old.it_value) && is_zero(old.it_interval));
    CHECK(timerfd_gettime(fd, &old) == 0);
    CHECK(!is_zero(old.it_value) && old.it_value.tv_sec <= 60);
    CHECK(old.it_interval.tv_sec == 0 && old.it_interval.tv_nsec == 20000000L);
    value = timer_value(0, 20000000L, 20000000L);
    CHECK(timerfd_settime(fd, 0, &value, &old) == 0);
    CHECK(!is_zero(old.it_value) && old.it_value.tv_sec <= 60);
    CHECK(old.it_interval.tv_sec == 0 && old.it_interval.tv_nsec == 20000000L);
    struct timespec delay = { .tv_sec = 0, .tv_nsec = 120000000L };
    while (nanosleep(&delay, &delay) < 0)
        CHECK(errno == EINTR);
    CHECK(read_count(fd) >= 3);
    CHECK(timerfd_gettime(fd, &old) == 0);
    CHECK(old.it_interval.tv_sec == 0 && old.it_interval.tv_nsec == 20000000L);
    CHECK(old.it_value.tv_sec == 0 && old.it_value.tv_nsec <= 20000000L);
    value = timer_value(0, 0, 0);
    CHECK(timerfd_settime(fd, 0, &value, &old) == 0);
    CHECK(old.it_interval.tv_sec == 0 && old.it_interval.tv_nsec == 20000000L);
    CHECK(timerfd_gettime(fd, &old) == 0);
    CHECK(is_zero(old.it_value) && is_zero(old.it_interval));
    empty(fd);
    poll_events(fd, 0, 0);
    value = timer_value(0, 20000000L, 0);
    CHECK(timerfd_settime(fd, 0, &value, NULL) == 0);
    poll_events(fd, POLLIN, 2000);
    value = timer_value(60, 0, 0);
    CHECK(timerfd_settime(fd, 0, &value, &old) == 0);
    CHECK(is_zero(old.it_value));
    empty(fd);
    poll_events(fd, 0, 0);
    value = timer_value(0, 0, 0);
    CHECK(timerfd_settime(fd, 0, &value, &old) == 0);
    CHECK(!is_zero(old.it_value));
    empty(fd);
    CHECK(close(fd) == 0);
    pass("timerfd-gettime-interval-disarm-rearm");
}

static void timerfd_shared(void) {
    int fd = timerfd_create(CLOCK_MONOTONIC, 0);
    CHECK(fd >= 0);
    int alias = dup(fd);
    CHECK(alias >= 0);
    struct itimerspec value = timer_value(0, 20000000L, 0);
    CHECK(timerfd_settime(alias, 0, &value, NULL) == 0);
    CHECK(read_count(fd) == 1);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        alarm(10);
        CHECK(timerfd_settime(alias, 0, &value, NULL) == 0);
        CHECK(read_count(alias) == 1);
        value = timer_value(60, 0, 20000000L);
        CHECK(timerfd_settime(alias, 0, &value, NULL) == 0);
        _exit(0);
    }
    child_ok(child);
    CHECK(timerfd_gettime(fd, &value) == 0);
    CHECK(!is_zero(value.it_value));
    CHECK(value.it_interval.tv_sec == 0 && value.it_interval.tv_nsec == 20000000L);
    CHECK(fcntl(fd, F_SETFL, O_NONBLOCK) == 0);
    empty(alias);
    CHECK(close(fd) == 0);
    value = timer_value(0, 20000000L, 0);
    CHECK(timerfd_settime(alias, 0, &value, NULL) == 0);
    poll_events(alias, POLLIN, 2000);
    CHECK(read_count(alias) == 1);
    CHECK(close(alias) == 0);
    pass("timerfd-dup-fork-blocking");
}

static void flags(int fd, int enabled) {
    int descriptor_flags = fcntl(fd, F_GETFD);
    CHECK(descriptor_flags >= 0);
    CHECK(!!(descriptor_flags & FD_CLOEXEC) == enabled);
    int status_flags = fcntl(fd, F_GETFL);
    CHECK(status_flags >= 0);
    CHECK(!!(status_flags & O_NONBLOCK) == enabled);
    int alias = dup(fd);
    CHECK(alias >= 0);
    CHECK(fcntl(alias, F_GETFD) == 0);
    CHECK(fcntl(alias, F_SETFL, status_flags ^ O_NONBLOCK) == 0);
    int changed = fcntl(fd, F_GETFL);
    CHECK(changed >= 0 && !!(changed & O_NONBLOCK) != enabled);
    CHECK(fcntl(alias, F_SETFL, status_flags) == 0);
    CHECK(close(alias) == 0);
}

static void exec_child(char **arguments) {
    int descriptors[4];
    for (int i = 0; i < 4; i++) {
        char *end;
        long fd = strtol(arguments[i], &end, 10);
        CHECK(*arguments[i] && !*end && fd >= 0 && fd <= INT32_MAX);
        descriptors[i] = (int)fd;
    }
    ERROR(fcntl(descriptors[0], F_GETFD), EBADF);
    ERROR(fcntl(descriptors[1], F_GETFD), EBADF);
    CHECK(read_count(descriptors[2]) == 23);
    write_count(descriptors[2], 29);
    struct itimerspec value;
    CHECK(timerfd_gettime(descriptors[3], &value) == 0);
    CHECK(!is_zero(value.it_value));
}

static void descriptor_flags(const char *program) {
    int descriptors[4] = {
        eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK),
        timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK),
        eventfd(23, 0),
        timerfd_create(CLOCK_MONOTONIC, 0)
    };
    char numbers[4][24];
    for (int i = 0; i < 4; i++) {
        CHECK(descriptors[i] >= 0);
        flags(descriptors[i], i < 2);
        CHECK(snprintf(numbers[i], sizeof numbers[i], "%d", descriptors[i]) > 0);
    }
    struct itimerspec value = timer_value(60, 0, 0);
    CHECK(timerfd_settime(descriptors[3], 0, &value, NULL) == 0);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        alarm(10);
        char *const arguments[] = {
            (char *)program, "--exec-child", numbers[0], numbers[1], numbers[2], numbers[3], NULL
        };
        execvp(program, arguments);
        fail("execvp", __LINE__);
    }
    child_ok(child);
    CHECK(read_count(descriptors[2]) == 29);
    for (int i = 0; i < 4; i++)
        CHECK(close(descriptors[i]) == 0);
    pass("descriptor-flags-exec");
}

static void timerfd_alarm_quota(void) {
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    CHECK(fd >= 0);
    struct itimerspec value = timer_value(3600, 0, 0);
    for (int i = 0; i < 1100; i++) {
        CHECK(timerfd_settime(fd, 0, &value, NULL) == 0);
    }
    CHECK(close(fd) == 0);
    for (int i = 0; i < 1024; i++) {
        int tfd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
        CHECK(tfd >= 0);
        CHECK(timerfd_settime(tfd, 0, &value, NULL) == 0);
        CHECK(close(tfd) == 0);
    }
    fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    CHECK(fd >= 0);
    CHECK(timerfd_settime(fd, 0, &value, NULL) == 0);
    CHECK(close(fd) == 0);
    pass("timerfd-alarm-quota");
}

static void timerfd_refresh_poll(void) {
    int fd = timerfd_create(CLOCK_MONOTONIC, TFD_NONBLOCK);
    CHECK(fd >= 0);
    int alias = dup(fd);
    CHECK(alias >= 0);
    for (int iter = 0; iter < 40; iter++) {
        struct itimerspec value = timer_value(0, 1000000L, 0);
        CHECK(timerfd_settime(fd, 0, &value, NULL) == 0);
        int ep = watch(fd, EPOLLIN);
        struct itimerspec cur;
        int expired = 0;
        for (int spin = 0; spin < 200000; spin++) {
            CHECK(timerfd_gettime(alias, &cur) == 0);
            if (cur.it_value.tv_sec == 0 && cur.it_value.tv_nsec == 0) {
                expired = 1;
                break;
            }
        }
        CHECK(expired == 1);
        struct epoll_event event = {0};
        CHECK(epoll_wait(ep, &event, 1, 200) == 1);
        CHECK((event.events & EPOLLIN) != 0);
        CHECK(read_count(fd) == 1);
        CHECK(close(ep) == 0);
    }
    CHECK(close(alias) == 0);
    CHECK(close(fd) == 0);
    pass("timerfd-refresh-poll");
}

int main(int argc, char **argv) {
    CHECK(setvbuf(stdout, NULL, _IONBF, 0) == 0);
    CHECK(signal(SIGALRM, timed_out) != SIG_ERR);
    alarm(45);
    if (argc == 6 && strcmp(argv[1], "--exec-child") == 0) {
        exec_child(argv + 2);
        return 0;
    }
    CHECK(argc == 1);
    eventfd_counter();
    eventfd_semaphore();
    eventfd_readiness();
    eventfd_shared();
    timerfd_errors();
    timerfd_clock(CLOCK_MONOTONIC, "timerfd-monotonic-relative-absolute-readiness");
    timerfd_clock(CLOCK_REALTIME, "timerfd-realtime-relative-absolute-readiness");
    timerfd_rearm();
    timerfd_shared();
    descriptor_flags(argv[0]);
    timerfd_alarm_quota();
    timerfd_refresh_poll();
    alarm(0);
    puts("fd-primitives-probe: PASS");
    return 0;
}
