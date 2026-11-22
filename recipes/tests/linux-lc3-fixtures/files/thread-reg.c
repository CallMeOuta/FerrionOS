#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

#define TGKILL_ROUNDS 200
#define MAIN_EXIT_ROUNDS 200
#define SPIN_LIMIT 50000000
#define PIPE_TIMEOUT_MS 5000

static atomic_int worker_release;

static void *hold_worker(void *arg) {
    (void)arg;
    for (int s = 0; s < SPIN_LIMIT && !atomic_load(&worker_release); s++) {
        if ((s & 1023) == 1023) {
            usleep(100);
        }
    }
    return NULL;
}

static void *pipe_worker(void *arg) {
    int fd = (int)(intptr_t)arg;
    usleep(15000);
    char b = 'k';
    (void)write(fd, &b, 1);
    close(fd);
    return NULL;
}

int main(void) {
    int tgkill_ok = 0;
    for (int r = 0; r < TGKILL_ROUNDS; r++) {
        atomic_store(&worker_release, 0);
        pthread_t t;
        if (pthread_create(&t, NULL, hold_worker, NULL) != 0) {
            printf("pthread_create failed at %d\n", r);
            return 2;
        }
        int err = pthread_kill(t, 0);
        atomic_store(&worker_release, 1);
        pthread_join(t, NULL);
        if (err == 0) {
            tgkill_ok++;
        }
    }
    printf("pthread_kill after create %d/%d\n", tgkill_ok, TGKILL_ROUNDS);

    int main_exit_ok = 0;
    for (int r = 0; r < MAIN_EXIT_ROUNDS; r++) {
        int p[2];
        if (pipe(p) != 0) {
            printf("pipe failed at %d\n", r);
            return 3;
        }
        fflush(stdout);
        pid_t pid = fork();
        if (pid < 0) {
            printf("fork failed at %d\n", r);
            return 3;
        }
        if (pid == 0) {
            close(p[0]);
            pthread_t t;
            if (pthread_create(&t, NULL, pipe_worker, (void *)(intptr_t)p[1]) != 0) {
                _exit(91);
            }
            pthread_exit(NULL);
        }
        close(p[1]);
        struct pollfd pfd = {.fd = p[0], .events = POLLIN, .revents = 0};
        char b = 0;
        int got = 0;
        if (poll(&pfd, 1, PIPE_TIMEOUT_MS) == 1 && read(p[0], &b, 1) == 1 && b == 'k') {
            got = 1;
        }
        close(p[0]);
        int st = 0;
        pid_t w = waitpid(pid, &st, 0);
        if (got && w == pid && WIFEXITED(st) && WEXITSTATUS(st) == 0) {
            main_exit_ok++;
        }
    }
    printf("main-exit worker ran %d/%d\n", main_exit_ok, MAIN_EXIT_ROUNDS);
    return 0;
}
