#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

struct subobj_box {
    char dst[16];
    char pad[64];
};

struct exact_box {
    char dst[16];
};

static struct exact_box global_target;
static char global_pad[64];

__attribute__((noinline))
static size_t dynamic_len(size_t v) {
    volatile size_t x = v;
    return x;
}

__attribute__((noinline))
static const char *dynamic_str(const char *s) {
    const char *volatile p = s;
    return p;
}

static int run_case(const char *name, void (*fn)(void)) {
    fflush(stdout);
    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "fortify-probe: fork failed: %s\n", strerror(errno));
        return 1;
    }
    if (pid == 0) {
        fn();
        printf("fortify-probe: %s SURVIVED\n", name);
        fflush(stdout);
        _exit(0);
    }
    int status = 0;
    pid_t waited = -1;
    for (int attempt = 0; attempt < 16; attempt++) {
        waited = waitpid(pid, &status, 0);
        if (waited >= 0 || errno != EINTR) {
            break;
        }
    }
    if (waited < 0) {
        fprintf(stderr, "fortify-probe: waitpid failed: %s\n", strerror(errno));
        return 1;
    }
    if (WIFSIGNALED(status)) {
        printf("fortify-probe: %s ABORTED sig=%d\n", name, WTERMSIG(status));
        return 0;
    }
    if (WIFEXITED(status)) {
        printf("fortify-probe: %s EXITED code=%d\n", name, WEXITSTATUS(status));
        return 1;
    }
    return 1;
}

static void overflow_memcpy(void) {
    char src[32];
    memset(src, 'A', sizeof(src));
    global_pad[0] = 0;
    memcpy(global_target.dst, src, dynamic_len(20));
    global_pad[1] = global_target.dst[0];
}

static void overflow_memmove(void) {
    char src[32];
    memset(src, 'B', sizeof(src));
    memmove(global_target.dst, src, dynamic_len(20));
    global_pad[1] = global_target.dst[0];
}

static void overflow_memset(void) {
    memset(global_target.dst, 'C', dynamic_len(20));
    global_pad[1] = global_target.dst[0];
}

static void overflow_strcpy(void) {
    struct subobj_box box;
    memset(&box, 0, sizeof(box));
    strcpy(box.dst, dynamic_str("0123456789abcdefXXXX"));
    global_pad[2] = box.dst[0];
}

static void overflow_strcat(void) {
    struct subobj_box box;
    memset(&box, 0, sizeof(box));
    strcpy(box.dst, dynamic_str("0123456789"));
    strcat(box.dst, dynamic_str("abcdefghij"));
    global_pad[2] = box.dst[0];
}

static void overflow_sprintf(void) {
    struct subobj_box box;
    memset(&box, 0, sizeof(box));
    sprintf(box.dst, "%s", dynamic_str("0123456789abcdefYYYY"));
    global_pad[2] = box.dst[0];
}

static void overflow_snprintf(void) {
    struct subobj_box box;
    memset(&box, 0, sizeof(box));
    snprintf(box.dst, dynamic_len(24), "%s", dynamic_str("short"));
    global_pad[2] = box.dst[0];
}

static void overflow_read(void) {
    int fds[2];
    if (pipe(fds) != 0) {
        _exit(2);
    }
    char data[24];
    memset(data, 'R', sizeof(data));
    ssize_t w = write(fds[1], data, sizeof(data));
    (void)w;
    ssize_t r = read(fds[0], global_target.dst, dynamic_len(20));
    (void)r;
    close(fds[0]);
    close(fds[1]);
}

static int check_valid_calls(void) {
    struct subobj_box box;
    memset(&box, 0, sizeof(box));
    memcpy(global_target.dst, "hello", dynamic_len(6));
    if (strcmp(global_target.dst, "hello") != 0) {
        return 1;
    }
    strcpy(box.dst, dynamic_str("abc"));
    strcat(box.dst, dynamic_str("def"));
    if (strcmp(box.dst, "abcdef") != 0) {
        return 1;
    }
    int n = snprintf(box.dst, dynamic_len(sizeof(box.dst)), "%s-%d", dynamic_str("ok"), 7);
    if (n != 4 || strcmp(box.dst, "ok-7") != 0) {
        return 1;
    }
    int fds[2];
    if (pipe(fds) != 0) {
        return 1;
    }
    if (write(fds[1], "ping", 4) != 4) {
        close(fds[0]);
        close(fds[1]);
        return 1;
    }
    ssize_t got = read(fds[0], global_target.dst, dynamic_len(4));
    close(fds[0]);
    close(fds[1]);
    if (got != 4 || memcmp(global_target.dst, "ping", 4) != 0) {
        return 1;
    }
    printf("fortify-probe: valid OK\n");
    return 0;
}

int main(void) {
    if (check_valid_calls() != 0) {
        printf("fortify-probe: valid FAIL\n");
        return 1;
    }
    int fail = 0;
    fail |= run_case("memcpy", overflow_memcpy);
    fail |= run_case("memmove", overflow_memmove);
    fail |= run_case("memset", overflow_memset);
    fail |= run_case("strcpy", overflow_strcpy);
    fail |= run_case("strcat", overflow_strcat);
    fail |= run_case("sprintf", overflow_sprintf);
    fail |= run_case("snprintf", overflow_snprintf);
    fail |= run_case("read", overflow_read);
    return fail ? 1 : 0;
}
