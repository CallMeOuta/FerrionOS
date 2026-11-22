#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(void) {
    char directory[] = "/tmp/directory-fsync-XXXXXX";
    if (!mkdtemp(directory)) {
        perror("mkdtemp");
        return 1;
    }
    char journal[128];
    snprintf(journal, sizeof(journal), "%s/journal", directory);
    const int flags[] = {O_RDONLY, O_RDONLY | O_DIRECTORY};
    const char *names[] = {"read-only", "directory-flag"};
    int failures = 0;
    for (size_t i = 0; i < sizeof(flags) / sizeof(flags[0]); ++i) {
        int directory_fd = open(directory, flags[i]);
        int file_fd = open(journal, O_CREAT | O_EXCL | O_WRONLY, 0600);
        if (directory_fd < 0 || file_fd < 0) {
            perror("open");
            if (directory_fd >= 0) close(directory_fd);
            if (file_fd >= 0) close(file_fd);
            unlink(journal);
            rmdir(directory);
            return 1;
        }
        int written = write(file_fd, "committed", 9) == 9 && fsync(file_fd) == 0;
        int closed = close(file_fd) == 0;
        int created_sync = fsync(directory_fd);
        int created_error = errno;
        int removed = unlink(journal) == 0;
        int removed_sync = fsync(directory_fd);
        int removed_error = errno;
        int directory_closed = close(directory_fd) == 0;
        int passed = written && closed && removed && directory_closed && created_sync == 0 && removed_sync == 0;
        printf("directory-fsync-probe: %s %s create=%d errno=%d unlink=%d errno=%d\n",
               passed ? "pass" : "FAIL", names[i], created_sync,
               created_sync ? created_error : 0, removed_sync,
               removed_sync ? removed_error : 0);
        failures += !passed;
    }
    errno = 0;
    int invalid = fsync(-1);
    int passed = invalid == -1 && errno == EBADF;
    printf("directory-fsync-probe: %s invalid\n", passed ? "pass" : "FAIL");
    failures += !passed;
    if (rmdir(directory)) {
        perror("rmdir");
        return 1;
    }
    printf("directory-fsync-probe: %s\n", failures ? "FAIL" : "PASS");
    return failures != 0;
}
