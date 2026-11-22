#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define NAME "fdtable-race-probe"
#define ROUNDS 20000

static int source_fd;

static void *dup_loop(void *arg)
{
    int target = (int)(long)arg;
    for (int i = 0; i < ROUNDS; i++) {
        if (dup2(source_fd, target) < 0) {
            return (void *)1;
        }
        int copy = fcntl(source_fd, F_DUPFD_CLOEXEC, 100);
        if (copy < 0 || close(copy) < 0) {
            return (void *)1;
        }
    }
    return NULL;
}

static void *open_loop(void *arg)
{
    (void)arg;
    for (int i = 0; i < ROUNDS; i++) {
        int fd = open("/scheme/null", O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
        }
        if (fd < 0) {
            return (void *)1;
        }
        int copy = dup(fd);
        if (copy < 0 || close(copy) < 0 || close(fd) < 0) {
            return (void *)1;
        }
    }
    return NULL;
}

int main(void)
{
    source_fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    if (source_fd < 0) {
        printf("%s: FAIL open\n", NAME);
        return 1;
    }
    pthread_t threads[6];
    for (int i = 0; i < 3; i++) {
        pthread_create(&threads[i], NULL, dup_loop, (void *)(long)(60 + i));
        pthread_create(&threads[3 + i], NULL, open_loop, NULL);
    }
    int failed = 0;
    for (int i = 0; i < 6; i++) {
        void *status;
        pthread_join(threads[i], &status);
        failed += status != NULL;
    }
    if (failed) {
        printf("%s: FAIL threads %d of 6 got an error\n", NAME, failed);
        return 1;
    }
    printf("%s: pass dup2-against-open\n", NAME);
    printf("%s: PASS\n", NAME);
    return 0;
}
