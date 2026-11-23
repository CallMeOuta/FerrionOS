#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/redox.h>
#include <unistd.h>

#define FD_CHECK_BOUND 256
#define DUP2_BASE 200
#define DUP2_SLOTS 4
#define PATH_BUF_SIZE 256
#define MAX_LEAK_PRINT 6

int main(void)
{
    assert(DUP2_BASE + DUP2_SLOTS <= FD_CHECK_BOUND);
    int leaks = 0;
    for (int fd = 3; fd < FD_CHECK_BOUND; fd++) {
        if (fd >= DUP2_BASE && fd < DUP2_BASE + DUP2_SLOTS) {
            continue;
        }
        errno = 0;
        int flags = fcntl(fd, F_GETFD);
        if (flags >= 0) {
            if (leaks < MAX_LEAK_PRINT) {
                char path_buf[PATH_BUF_SIZE];
                ssize_t n = redox_fpath(fd, path_buf, sizeof(path_buf) - 1);
                if (n > 0 && (size_t)n < sizeof(path_buf)) {
                    path_buf[n] = '\0';
                } else {
                    path_buf[0] = '?';
                    path_buf[1] = '\0';
                }
                fprintf(stderr, "fork-filetable-checker: leaked fd=%d flags=0x%x path=%s\n", fd, flags, path_buf);
            }
            leaks++;
        }
    }
    if (leaks > MAX_LEAK_PRINT) {
        fprintf(stderr, "fork-filetable-checker: leaked total=%d\n", leaks);
    }
    if (leaks > 0) {
        return 42;
    }
    return 0;
}
