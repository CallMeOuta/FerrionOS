/* signal-handle-probe: checks that signals sent through process handles follow kill()'s
 * permission rule (security roadmap S12).
 *
 * Run it as a non-root member of group sudo whose password is empty (`user` on dev images). It
 * starts a session, has a child become root through sudo inside it, then sends that child SIGTERM
 * twice: with kill(), and through a process handle (`child/<pid>/proc-obj`, which a session
 * leader may open for any process of its session). It prints REFUSED and exits 0 when both are
 * refused with EPERM, and ACCEPTED and exits 1 when the handle delivers the signal. Other
 * outcomes print INCONCLUSIVE and exit 2.
 *
 * Checked by `scripts/vm-test.py signal-handle`. */
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

/* relibc's C interface to the process manager. */
size_t redox_cur_procfd_v0(void);
size_t redox_openat_v1(size_t fd, const char *path, size_t len, unsigned flags, unsigned fcntl_flags);
size_t redox_sys_call_v0(size_t fd, unsigned char *payload, size_t len, size_t flags, const uint64_t *meta,
                         size_t meta_len);

/* PidfdCall::SendSignal of the process manager's protocol. */
#define PIDFD_SEND_SIGNAL 0

static const char *result(long r) {
    return r == 0 ? "ok" : r == -EPERM ? "EPERM" : strerror((int)-r);
}

static void sleep_ms(long ms) {
    struct timespec t = {ms / 1000, (ms % 1000) * 1000000};
    nanosleep(&t, NULL);
}

static int probe(void) {
    if (getuid() == 0) {
        puts("signal-handle-probe: run it as a non-root user");
        return 2;
    }
    if (setsid() < 0) {
        printf("signal-handle-probe: setsid failed: %s\n", strerror(errno));
        return 2;
    }
    int pipe_fds[2];
    if (pipe(pipe_fds) < 0)
        return 2;
    pid_t child = fork();
    if (child == 0) {
        dup2(pipe_fds[0], 0);
        close(pipe_fds[0]);
        close(pipe_fds[1]);
        execlp("sudo", "sudo", "sleep", "20", (char *)NULL);
        _exit(127);
    }
    close(pipe_fds[0]);
    /* `user`'s password is empty, so sudo runs without reading this newline; it
       stays for images built before sudo stopped reading its standard input. */
    if (write(pipe_fds[1], "\n", 1) != 1)
        return 2;
    int root = 0;
    for (int i = 0; i < 100 && !root; i++) {
        sleep_ms(100);
        root = kill(child, 0) < 0 && errno == EPERM;
    }
    if (!root) {
        puts("signal-handle-probe: the child did not become root through sudo");
        puts("signal-handle-probe: INCONCLUSIVE");
        return 2;
    }
    long direct = kill(child, SIGTERM) < 0 ? -errno : 0;

    char path[64];
    int len = snprintf(path, sizeof path, "child/%d/proc-obj", (int)child);
    long handle = (long)redox_openat_v1(redox_cur_procfd_v0(), path, (size_t)len, 0, 0);
    long through_handle = handle;
    if (handle >= 0) {
        uint64_t metadata[2] = {PIDFD_SEND_SIGNAL, SIGTERM};
        long r = (long)redox_sys_call_v0((size_t)handle, NULL, 0, 0, metadata, 2);
        through_handle = r < 0 ? r : 0;
    }
    sleep_ms(500);
    int status = 0;
    pid_t done = waitpid(child, &status, WNOHANG);
    const char *fate = done == child && WIFSIGNALED(status) ? "killed" : done == child ? "exited" : "running";
    printf("signal-handle-probe: uid %d, root child %d: kill %s, handle open %s, handle signal %s, child %s\n",
           (int)getuid(), (int)child, result(direct), handle >= 0 ? "ok" : result(handle),
           handle >= 0 ? result(through_handle) : "-", fate);
    if (handle >= 0 && through_handle == 0) {
        puts("signal-handle-probe: ACCEPTED");
        return 1;
    }
    if (direct == -EPERM && handle >= 0 && through_handle == -EPERM) {
        puts("signal-handle-probe: REFUSED");
        return 0;
    }
    puts("signal-handle-probe: INCONCLUSIVE");
    return 2;
}

int main(void) {
    /* A process group leader cannot start a session, and a shell makes each command one, so the
     * probe runs in a child. */
    pid_t worker = fork();
    if (worker == 0) {
        int r = probe();
        fflush(stdout);
        _exit(r);
    }
    int status = 0;
    waitpid(worker, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : 2;
}
