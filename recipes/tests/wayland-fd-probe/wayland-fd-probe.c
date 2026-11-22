#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

static const char KEYMAP[] = "xkb_keymap {\n\txkb_keycodes \"evdev\" { };\n};\n";

static int failures;

static void result(const char *name, int ok, const char *detail)
{
    printf("WAYLAND-FD %s %s%s%s\n", ok ? "PASS" : "FAIL", name, detail[0] ? ": " : "", detail);
    if (!ok) {
        failures++;
    }
}

static void check_map(const char *name, const char *seen, size_t size)
{
    char detail[96] = "";
    int ok = seen != MAP_FAILED && memcmp(seen, KEYMAP, size) == 0;
    if (seen == MAP_FAILED) {
        snprintf(detail, sizeof(detail), "mmap: %s", strerror(errno));
    } else if (!ok) {
        snprintf(detail, sizeof(detail), "first bytes %02x %02x %02x %02x", (unsigned char)seen[0],
                 (unsigned char)seen[1], (unsigned char)seen[2], (unsigned char)seen[3]);
    }
    result(name, ok, detail);
}

static int send_fd(int sock, int fd)
{
    char byte = 0;
    struct iovec iov = {&byte, 1};
    union {
        char buf[CMSG_SPACE(sizeof(int))];
        struct cmsghdr align;
    } control;
    memset(&control, 0, sizeof(control));
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.buf;
    msg.msg_controllen = sizeof(control.buf);
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cmsg), &fd, sizeof(int));
    return sendmsg(sock, &msg, 0) == 1 ? 0 : -1;
}

static int recv_fd(int sock)
{
    char byte;
    struct iovec iov = {&byte, 1};
    union {
        char buf[CMSG_SPACE(sizeof(int))];
        struct cmsghdr align;
    } control;
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.buf;
    msg.msg_controllen = sizeof(control.buf);
    if (recvmsg(sock, &msg, MSG_CMSG_CLOEXEC) != 1) {
        return -1;
    }
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    if (!cmsg || cmsg->cmsg_type != SCM_RIGHTS) {
        return -1;
    }
    int fd;
    memcpy(&fd, CMSG_DATA(cmsg), sizeof(int));
    return fd;
}

static void keymap_checks(const char *kind, int close_writer)
{
    size_t size = sizeof(KEYMAP);
    char name[64];
    char check[96];
    snprintf(name, sizeof(name), "/wayland-fd-probe-%d-%s", (int)getpid(), kind);
    int rw = shm_open(name, O_CREAT | O_EXCL | O_RDWR | O_CLOEXEC, 0600);
    int ro = shm_open(name, O_RDONLY | O_CLOEXEC, 0);
    shm_unlink(name);
    if (rw < 0 || ro < 0 || write(rw, KEYMAP, size) != (ssize_t)size) {
        result(kind, 0, strerror(errno));
        return;
    }
    if (close_writer) {
        close(rw);
    } else {
        snprintf(check, sizeof(check), "%s-shared-rw", kind);
        check_map(check, mmap(NULL, size, PROT_READ, MAP_SHARED, rw, 0), size);
    }
    snprintf(check, sizeof(check), "%s-shared-ro", kind);
    check_map(check, mmap(NULL, size, PROT_READ, MAP_SHARED, ro, 0), size);
    snprintf(check, sizeof(check), "%s-private-ro", kind);
    check_map(check, mmap(NULL, size, PROT_READ, MAP_PRIVATE, ro, 0), size);

    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) != 0) {
        result(kind, 0, strerror(errno));
        return;
    }
    pid_t child = fork();
    if (child == 0) {
        close(sv[0]);
        int fd = recv_fd(sv[1]);
        if (fd < 0) {
            result(kind, 0, "no descriptor received");
            _exit(1);
        }
        snprintf(check, sizeof(check), "%s-passed-private", kind);
        check_map(check, mmap(NULL, size, PROT_READ, MAP_PRIVATE, fd, 0), size);
        snprintf(check, sizeof(check), "%s-passed-shared", kind);
        check_map(check, mmap(NULL, size, PROT_READ, MAP_SHARED, fd, 0), size);
        fflush(stdout);
        _exit(failures ? 1 : 0);
    }
    close(sv[1]);
    if (send_fd(sv[0], ro) != 0) {
        result(kind, 0, strerror(errno));
    }
    int status = 0;
    waitpid(child, &status, 0);
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        failures++;
    }
    close(sv[0]);
    if (!close_writer) {
        close(rw);
    }
    close(ro);
}

static void idle_checks(const char *kind, int reader, int writer)
{
    char name[96];
    char detail[96];
    struct pollfd pfd = {reader, POLLIN, 0};

    snprintf(name, sizeof(name), "%s-poll-empty", kind);
    int ready = poll(&pfd, 1, 100);
    snprintf(detail, sizeof(detail), "poll returned %d revents 0x%x", ready, pfd.revents);
    result(name, ready == 0, ready == 0 ? "" : detail);

    char data[4] = {1, 2, 3, 4};
    char got[16];
    if (write(writer, data, sizeof(data)) != (ssize_t)sizeof(data)) {
        result(kind, 0, strerror(errno));
        return;
    }
    pfd.revents = 0;
    snprintf(name, sizeof(name), "%s-poll-data", kind);
    ready = poll(&pfd, 1, 1000);
    snprintf(detail, sizeof(detail), "poll returned %d revents 0x%x", ready, pfd.revents);
    result(name, ready == 1 && (pfd.revents & POLLIN), ready == 1 && (pfd.revents & POLLIN) ? "" : detail);

    ssize_t n = recv(reader, got, sizeof(got), MSG_DONTWAIT);
    snprintf(name, sizeof(name), "%s-read", kind);
    snprintf(detail, sizeof(detail), "recv returned %zd", n);
    result(name, n == (ssize_t)sizeof(data), n == (ssize_t)sizeof(data) ? "" : detail);

    pfd.revents = 0;
    snprintf(name, sizeof(name), "%s-poll-drained", kind);
    ready = poll(&pfd, 1, 100);
    snprintf(detail, sizeof(detail), "poll returned %d revents 0x%x", ready, pfd.revents);
    result(name, ready == 0, ready == 0 ? "" : detail);

    n = recv(reader, got, sizeof(got), MSG_DONTWAIT);
    int again = errno;
    pfd.revents = 0;
    snprintf(name, sizeof(name), "%s-poll-after-eagain", kind);
    ready = poll(&pfd, 1, 100);
    snprintf(detail, sizeof(detail), "recv %zd errno %d, poll returned %d revents 0x%x", n, again, ready, pfd.revents);
    result(name, n < 0 && again == EAGAIN && ready == 0, n < 0 && again == EAGAIN && ready == 0 ? "" : detail);

    int ep = epoll_create1(EPOLL_CLOEXEC);
    struct epoll_event ev = {0};
    ev.events = EPOLLIN;
    ev.data.fd = reader;
    struct epoll_event out[4];
    snprintf(name, sizeof(name), "%s-epoll-drained", kind);
    if (ep < 0 || epoll_ctl(ep, EPOLL_CTL_ADD, reader, &ev) != 0) {
        result(name, 0, strerror(errno));
        return;
    }
    ready = epoll_wait(ep, out, 4, 100);
    snprintf(detail, sizeof(detail), "epoll_wait returned %d events 0x%x", ready, ready > 0 ? out[0].events : 0);
    result(name, ready == 0, ready == 0 ? "" : detail);
    close(ep);
}

struct connector {
    int fd;
    struct sockaddr_un addr;
    int status;
};

static void *connect_thread(void *arg)
{
    struct connector *connector = arg;
    connector->status = connect(connector->fd, (struct sockaddr *)&connector->addr, sizeof(connector->addr));
    return NULL;
}

static void poll_checks(void)
{
    int sv[2];
    if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv) != 0) {
        result("socketpair", 0, strerror(errno));
    } else {
        idle_checks("socketpair", sv[0], sv[1]);
        close(sv[0]);
        close(sv[1]);
    }

    struct sockaddr_un addr = {0};
    addr.sun_family = AF_UNIX;
    snprintf(addr.sun_path, sizeof(addr.sun_path), "/tmp/wayland-fd-probe-%d.sock", (int)getpid());
    unlink(addr.sun_path);
    int listener = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    struct connector connector = {socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0), addr, -1};
    pthread_t thread;
    if (listener < 0 || connector.fd < 0 || bind(listener, (struct sockaddr *)&addr, sizeof(addr)) != 0 ||
        listen(listener, 4) != 0 || pthread_create(&thread, NULL, connect_thread, &connector) != 0) {
        result("accepted", 0, strerror(errno));
        return;
    }
    int server = accept(listener, NULL, NULL);
    pthread_join(thread, NULL);
    int client = connector.fd;
    if (server < 0 || connector.status != 0) {
        result("accepted", 0, strerror(errno));
        return;
    }
    fcntl(server, F_SETFL, fcntl(server, F_GETFL) | O_NONBLOCK);
    idle_checks("accepted", server, client);
    idle_checks("connected", client, server);
    close(server);
    close(client);
    close(listener);
    unlink(addr.sun_path);
}

int main(void)
{
    keymap_checks("keymap", 0);
    keymap_checks("keymap-writer-closed", 1);
    poll_checks();
    printf("WAYLAND-FD %s\n", failures ? "FAILED" : "OK");
    return failures ? 1 : 0;
}
