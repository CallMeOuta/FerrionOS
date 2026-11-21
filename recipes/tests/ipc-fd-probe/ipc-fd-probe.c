#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#define SHM_SIZE (256 * 1024)
#define CHILD_FD 7
#define MAX_FDS 16
#define BURST 32
#define BIG (1024 * 1024)

static const char *self_path;

static void fail(const char *expression, int line) {
    fprintf(stderr, "ipc-fd-probe: FAIL line %d: %s (errno %d: %s)\n", line, expression, errno,
            strerror(errno));
    exit(1);
}

#define CHECK(expression) do { if (!(expression)) fail(#expression, __LINE__); } while (0)

static void timed_out(int signal_number) {
    static const char message[] = "ipc-fd-probe: FAIL alarm timeout\n";
    (void)signal_number;
    (void)write(STDERR_FILENO, message, sizeof message - 1);
    _exit(124);
}

static void pass(const char *name) {
    printf("ipc-fd-probe: pass %s\n", name);
    fflush(stdout);
}

static void set_nonblocking(int fd) {
    int flags = fcntl(fd, F_GETFL);
    CHECK(flags >= 0);
    CHECK(fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0);
}

static int create_shm(int *read_only) {
    static int counter;
    char name[64];
    int fd;
    do {
        snprintf(name, sizeof name, "/org.mozilla.ipc.%d.%d", (int)getpid(), counter++);
        fd = shm_open(name, O_RDWR | O_CREAT | O_EXCL, 0600);
    } while (fd < 0 && errno == EEXIST);
    CHECK(fd >= 0);
    *read_only = shm_open(name, O_RDONLY, 0400);
    CHECK(*read_only >= 0);
    CHECK(shm_unlink(name) == 0);
    CHECK(posix_fallocate(fd, 0, SHM_SIZE) == 0);
    struct stat st;
    CHECK(fstat(fd, &st) == 0 && st.st_size == SHM_SIZE);
    CHECK(fstat(*read_only, &st) == 0 && st.st_size == SHM_SIZE);
    return fd;
}

static ssize_t send_with_fds(int sock, const void *data, size_t len, const int *fds, size_t nfds) {
    char control[CMSG_SPACE(sizeof(int) * MAX_FDS)];
    struct iovec iov = {(void *)data, len};
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    if (nfds) {
        memset(control, 0, sizeof control);
        msg.msg_control = control;
        msg.msg_controllen = CMSG_LEN(sizeof(int) * nfds);
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
        cmsg->cmsg_level = SOL_SOCKET;
        cmsg->cmsg_type = SCM_RIGHTS;
        cmsg->cmsg_len = msg.msg_controllen;
        memcpy(CMSG_DATA(cmsg), fds, sizeof(int) * nfds);
    }
    return sendmsg(sock, &msg, MSG_DONTWAIT);
}

static ssize_t recv_with_fds(int sock, void *data, size_t len, int *fds, size_t *nfds) {
    char control[CMSG_SPACE(sizeof(int) * MAX_FDS) + 32];
    struct iovec iov = {data, len};
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = sizeof control;
    ssize_t n = recvmsg(sock, &msg, MSG_DONTWAIT | MSG_CMSG_CLOEXEC);
    if (n < 0) {
        return n;
    }
    CHECK(!(msg.msg_flags & MSG_CTRUNC));
    if (msg.msg_controllen > 0) {
        for (struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg); cmsg; cmsg = CMSG_NXTHDR(&msg, cmsg)) {
            if (cmsg->cmsg_level == SOL_SOCKET && cmsg->cmsg_type == SCM_RIGHTS) {
                size_t count = (cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int);
                memcpy(fds + *nfds, CMSG_DATA(cmsg), count * sizeof(int));
                *nfds += count;
            }
        }
    }
    return n;
}

static void wait_readable(int sock) {
    int ep = epoll_create1(EPOLL_CLOEXEC);
    CHECK(ep >= 0);
    struct epoll_event ev = {.events = EPOLLIN, .data.fd = sock};
    CHECK(epoll_ctl(ep, EPOLL_CTL_ADD, sock, &ev) == 0);
    struct epoll_event got;
    int n;
    do {
        n = epoll_wait(ep, &got, 1, 10000);
    } while (n < 0 && errno == EINTR);
    CHECK(n == 1 && (got.events & EPOLLIN));
    close(ep);
}

static void read_all(int sock, char *buf, size_t len, size_t chunk, int *fds, size_t *nfds) {
    size_t got = 0;
    while (got < len) {
        size_t want = len - got < chunk ? len - got : chunk;
        ssize_t n = recv_with_fds(sock, buf + got, want, fds, nfds);
        if (n < 0 && errno == EAGAIN) {
            wait_readable(sock);
            continue;
        }
        CHECK(n > 0);
        got += (size_t)n;
    }
}

static int child_main(void) {
    int sock = CHILD_FD;
    char buf[64];
    int fds[MAX_FDS];
    size_t nfds = 0;
    read_all(sock, buf, 5, 5, fds, &nfds);
    CHECK(memcmp(buf, "SHMEM", 5) == 0);
    CHECK(nfds == 2);
    for (size_t i = 0; i < nfds; i++) {
        int flags = fcntl(fds[i], F_GETFD);
        CHECK(flags >= 0 && (flags & FD_CLOEXEC));
    }
    char *rw = mmap(NULL, SHM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, fds[0], 0);
    CHECK(rw != MAP_FAILED);
    const char *ro = mmap(NULL, SHM_SIZE, PROT_READ, MAP_SHARED, fds[1], 0);
    CHECK(ro != MAP_FAILED);
    CHECK(strcmp(rw, "parent wrote this") == 0);
    CHECK(strcmp(ro, "parent wrote this") == 0);
    CHECK(rw[SHM_SIZE - 1] == 'P');
    strcpy(rw + 4096, "child wrote this");
    CHECK(strcmp(ro + 4096, "child wrote this") == 0);
    CHECK(send_with_fds(sock, "DONE!", 5, NULL, 0) == 5);
    return 0;
}

int main(int argc, char **argv) {
    signal(SIGALRM, timed_out);
    alarm(60);
    self_path = argv[0];
    if (argc > 1 && strcmp(argv[1], "child") == 0) {
        return child_main();
    }

    int ro_fd;
    int shm_fd = create_shm(&ro_fd);
    char *map = mmap(NULL, SHM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED, shm_fd, 0);
    CHECK(map != MAP_FAILED);
    strcpy(map, "parent wrote this");
    map[SHM_SIZE - 1] = 'P';
    const char *ro_map = mmap(NULL, SHM_SIZE, PROT_READ, MAP_SHARED, ro_fd, 0);
    CHECK(ro_map != MAP_FAILED && strcmp(ro_map, "parent wrote this") == 0);
    pass("shm-create");

    int sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0);
    set_nonblocking(sv[0]);
    set_nonblocking(sv[1]);
    char buf[4096];
    int fds[BURST * 2 + MAX_FDS];
    size_t nfds = 0;
    errno = 0;
    CHECK(recv_with_fds(sv[1], buf, sizeof buf, fds, &nfds) == -1 && errno == EAGAIN);
    pass("empty-eagain");

    int pair[2] = {shm_fd, ro_fd};
    CHECK(send_with_fds(sv[0], "hello", 5, pair, 2) == 5);
    CHECK(recv_with_fds(sv[1], buf, sizeof buf, fds, &nfds) == 5 && memcmp(buf, "hello", 5) == 0);
    CHECK(nfds == 2);
    for (size_t i = 0; i < nfds; i++) {
        int flags = fcntl(fds[i], F_GETFD);
        CHECK(flags >= 0 && (flags & FD_CLOEXEC));
    }
    const char *seen = mmap(NULL, SHM_SIZE, PROT_READ, MAP_SHARED, fds[0], 0);
    CHECK(seen != MAP_FAILED && strcmp(seen, "parent wrote this") == 0);
    munmap((void *)seen, SHM_SIZE);
    close(fds[0]);
    close(fds[1]);
    nfds = 0;
    pass("scm-rights-cloexec");

    for (int i = 0; i < BURST; i++) {
        char msg[100];
        memset(msg, 'a' + i % 26, sizeof msg);
        CHECK(send_with_fds(sv[0], msg, sizeof msg, &ro_fd, 1) == (ssize_t)sizeof msg);
    }
    static char burst[BURST * 100];
    size_t got = 0;
    while (got < sizeof burst) {
        ssize_t n = recv_with_fds(sv[1], burst + got, 37, fds, &nfds);
        CHECK(n > 0);
        got += (size_t)n;
        CHECK(nfds >= (got + 99) / 100);
    }
    CHECK(nfds == BURST);
    for (size_t i = 0; i < nfds; i++) {
        close(fds[i]);
    }
    nfds = 0;
    for (int i = 0; i < BURST; i++) {
        CHECK(burst[i * 100] == 'a' + i % 26 && burst[i * 100 + 99] == 'a' + i % 26);
    }
    pass("burst-partial-reads");

    char *big = malloc(BIG);
    char *back = malloc(BIG);
    CHECK(big && back);
    for (size_t i = 0; i < BIG; i++) {
        big[i] = (char)(i * 7);
    }
    size_t sent = 0, received = 0;
    while (received < BIG) {
        if (sent < BIG) {
            ssize_t n = send_with_fds(sv[0], big + sent, BIG - sent, NULL, 0);
            CHECK(n > 0 || errno == EAGAIN);
            if (n > 0) {
                sent += (size_t)n;
            }
        }
        size_t want = BIG - received < 4096 ? BIG - received : 4096;
        ssize_t r = recv_with_fds(sv[1], back + received, want, fds, &nfds);
        CHECK(r > 0 || errno == EAGAIN);
        if (r > 0) {
            received += (size_t)r;
        }
        CHECK(received <= sent);
    }
    CHECK(nfds == 0 && memcmp(big, back, BIG) == 0);
    pass("large-message");

    CHECK(send_with_fds(sv[0], "0123456789", 10, NULL, 0) == 10);
    wait_readable(sv[1]);
    CHECK(recv_with_fds(sv[1], buf, 4, fds, &nfds) == 4);
    wait_readable(sv[1]);
    CHECK(recv_with_fds(sv[1], buf, sizeof buf, fds, &nfds) == 6);
    pass("epoll-level");

    int child_sv[2];
    CHECK(socketpair(AF_UNIX, SOCK_STREAM, 0, child_sv) == 0);
    set_nonblocking(child_sv[0]);
    set_nonblocking(child_sv[1]);
    CHECK(fcntl(child_sv[0], F_SETFD, FD_CLOEXEC) == 0);
    CHECK(send_with_fds(child_sv[0], "SHMEM", 5, pair, 2) == 5);
    pid_t pid = fork();
    CHECK(pid >= 0);
    if (pid == 0) {
        if (dup2(child_sv[1], CHILD_FD) < 0) {
            _exit(126);
        }
        execlp(self_path, self_path, "child", (char *)NULL);
        _exit(127);
    }
    close(child_sv[1]);
    nfds = 0;
    read_all(child_sv[0], buf, 5, 5, fds, &nfds);
    CHECK(memcmp(buf, "DONE!", 5) == 0);
    int status = 0;
    CHECK(waitpid(pid, &status, 0) == pid);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(strcmp(map + 4096, "child wrote this") == 0);
    pass("cross-process-shm");

    printf("ipc-fd-probe: PASS\n");
    return 0;
}
