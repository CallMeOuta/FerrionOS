#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

enum {
    RECORD_BYTES = 128 * 1024,
    RANDOM_PREFIX_BYTES = 2048,
    BASE_RECORDS = 128,
    PHASE_SPAN = 64,
    MAX_RECORDS = BASE_RECORDS + PHASE_SPAN,
    MAX_BYTES = MAX_RECORDS * RECORD_BYTES,
    SMALL_BYTES = 4096,
    MAX_SMALL_WRITERS = 4,
    SMALL_PRINT_EVERY = 100,
    MAX_SMALL_ROUNDS = 500000,
    DEFAULT_SECONDS = 300,
    MAX_SECONDS = 3600,
    MAX_ROUNDS = 100000,
    MAX_WRITE_CALLS = 256,
};

static uint64_t monotonic_ms(void) {
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)(ts.tv_nsec / 1000000L);
}

static uint64_t xorshift64(uint64_t *state) {
    uint64_t x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    return x;
}

static void fill_records(uint8_t *buf, size_t records, uint32_t round) {
    uint64_t state = 0x9e3779b97f4a7c15ULL ^ ((uint64_t)(round + 1u) * 0xbf58476d1ce4e5b9ULL);
    if (state == 0) {
        state = 1;
    }
    for (size_t rec = 0; rec < records; ++rec) {
        uint8_t *dst = buf + rec * RECORD_BYTES;
        for (size_t off = 0; off < RANDOM_PREFIX_BYTES; off += sizeof(uint64_t)) {
            uint64_t v = xorshift64(&state);
            memcpy(dst + off, &v, sizeof(v));
        }
        memset(dst + RANDOM_PREFIX_BYTES, 0x5a, RECORD_BYTES - RANDOM_PREFIX_BYTES);
    }
}

static int write_all(int fd, const uint8_t *buf, size_t total) {
    size_t written = 0;
    for (int calls = 0; calls < MAX_WRITE_CALLS && written < total; ++calls) {
        ssize_t w = write(fd, buf + written, total - written);
        if (w < 0) {
            if (errno == EINTR) {
                continue;
            }
            perror("write");
            return -1;
        }
        if (w == 0) {
            fprintf(stderr, "nvme-queue-probe: zero-byte write\n");
            return -1;
        }
        written += (size_t)w;
    }
    if (written != total) {
        fprintf(stderr, "nvme-queue-probe: short write %zu of %zu\n", written, total);
        return -1;
    }
    return 0;
}

static int run_small_writer(const char *dir, int id, uint64_t start_ms, uint64_t deadline_ms) {
    char path[128];
    int n = snprintf(path, sizeof(path), "%s/small-%d.bin", dir, id);
    assert(n > 0 && (size_t)n < sizeof(path));

    uint8_t small_buf[SMALL_BYTES];
    uint64_t state = 0x243f6a8885a308d3ULL ^ ((uint64_t)(id + 1) * 0x9e3779b97f4a7c15ULL);
    if (state == 0) {
        state = 1;
    }
    for (size_t off = 0; off < SMALL_BYTES; off += sizeof(uint64_t)) {
        uint64_t v = xorshift64(&state);
        memcpy(small_buf + off, &v, sizeof(v));
    }

    for (uint32_t round = 0; round < MAX_SMALL_ROUNDS; ++round) {
        if (monotonic_ms() >= deadline_ms) {
            break;
        }
        small_buf[0] = (uint8_t)(round & 0xffu);
        int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600);
        if (fd < 0) {
            perror("open");
            return 1;
        }
        if (write_all(fd, small_buf, SMALL_BYTES) != 0) {
            close(fd);
            unlink(path);
            return 1;
        }
        if (fsync(fd) != 0) {
            perror("fsync");
            close(fd);
            unlink(path);
            return 1;
        }
        if (close(fd) != 0) {
            perror("close");
            unlink(path);
            return 1;
        }
        if (unlink(path) != 0) {
            perror("unlink");
            return 1;
        }
        if ((round + 1u) % SMALL_PRINT_EVERY == 0) {
            printf("NVMEQ-SMALL %d %u %llu\n", id, round + 1u,
                   (unsigned long long)(monotonic_ms() - start_ms));
            fflush(stdout);
        }
    }
    return 0;
}

int main(int argc, char **argv) {
    uint64_t duration_s = DEFAULT_SECONDS;
    int small_writers = 0;
    if (argc > 1) {
        char *end = NULL;
        unsigned long parsed = strtoul(argv[1], &end, 10);
        if (end == argv[1] || *end != '\0' || parsed == 0 || parsed > MAX_SECONDS) {
            fprintf(stderr, "nvme-queue-probe: invalid duration: %s\n", argv[1]);
            return 2;
        }
        duration_s = (uint64_t)parsed;
    }
    if (argc > 2) {
        char *end = NULL;
        unsigned long parsed = strtoul(argv[2], &end, 10);
        if (end == argv[2] || *end != '\0' || parsed > MAX_SMALL_WRITERS) {
            fprintf(stderr, "nvme-queue-probe: invalid small-writers: %s\n", argv[2]);
            return 2;
        }
        small_writers = (int)parsed;
    }

    uint8_t *buf = malloc(MAX_BYTES);
    if (!buf) {
        perror("malloc");
        return 1;
    }

    char dir[] = "/tmp/nvme-queue-XXXXXX";
    if (!mkdtemp(dir)) {
        perror("mkdtemp");
        free(buf);
        return 1;
    }
    char path[128];
    int n = snprintf(path, sizeof(path), "%s/burst.bin", dir);
    assert(n > 0 && (size_t)n < sizeof(path));

    uint64_t start_ms = monotonic_ms();
    uint64_t deadline_ms = start_ms + duration_s * 1000u;

    pid_t kids[MAX_SMALL_WRITERS];
    for (int i = 0; i < small_writers; ++i) {
        pid_t pid = fork();
        if (pid < 0) {
            perror("fork");
            for (int j = 0; j < i; ++j) {
                waitpid(kids[j], NULL, 0);
            }
            rmdir(dir);
            free(buf);
            return 1;
        }
        if (pid == 0) {
            free(buf);
            _exit(run_small_writer(dir, i, start_ms, deadline_ms));
        }
        kids[i] = pid;
    }

    int failed = 0;
    uint32_t round = 0;
    for (round = 0; round < MAX_ROUNDS; ++round) {
        if (monotonic_ms() >= deadline_ms) {
            break;
        }
        size_t records = BASE_RECORDS + (round % PHASE_SPAN);
        size_t total = records * RECORD_BYTES;
        assert(total <= MAX_BYTES);
        fill_records(buf, records, round);

        int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0600);
        if (fd < 0) {
            perror("open");
            failed = 1;
            break;
        }
        if (write_all(fd, buf, total) != 0) {
            close(fd);
            unlink(path);
            failed = 1;
            break;
        }
        if (fsync(fd) != 0) {
            perror("fsync");
            close(fd);
            unlink(path);
            failed = 1;
            break;
        }
        if (close(fd) != 0) {
            perror("close");
            unlink(path);
            failed = 1;
            break;
        }
        if (unlink(path) != 0) {
            perror("unlink");
            failed = 1;
            break;
        }
        printf("NVMEQ %u %llu\n", round, (unsigned long long)(monotonic_ms() - start_ms));
        fflush(stdout);
    }

    for (int i = 0; i < small_writers; ++i) {
        int status = 0;
        if (waitpid(kids[i], &status, 0) < 0 || !WIFEXITED(status) || WEXITSTATUS(status) != 0) {
            failed = 1;
        }
    }

    if (rmdir(dir) != 0) {
        perror("rmdir");
        failed = 1;
    }
    free(buf);
    if (failed) {
        return 1;
    }
    uint64_t elapsed_ms = monotonic_ms() - start_ms;
    printf("NVMEQ-DONE rounds=%u elapsed_ms=%llu\n", round, (unsigned long long)elapsed_ms);
    fflush(stdout);
    return 0;
}
