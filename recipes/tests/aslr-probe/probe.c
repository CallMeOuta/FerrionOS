#define _GNU_SOURCE
#include <dlfcn.h>
#include <elf.h>
#include <errno.h>
#include <inttypes.h>
#include <pthread.h>
#include <spawn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/mman.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern char **environ;
int main(int argc, char **argv);

enum {
    WAIT_RETRIES = 16,
    BENCH_WARMUP_RUNS = 4,
    MAX_BENCH_RUNS = 256,
};

struct sample_data {
    uint64_t main_addr;
    uint64_t stack_addr;
    uint64_t stack_base;
    uint64_t stack_size;
    uint64_t malloc_addr;
    uint64_t mmap_addr;
    uint64_t dso_addr;
    uint64_t at_random_ptr;
    uint64_t at_random_val;
};

static int collect_sample(struct sample_data *out, int keep_dso, void **dso_handle_out) {
    volatile uint64_t local_var = 0x123456789abcdef0ULL;
    memset(out, 0, sizeof(*out));
    out->main_addr = (uint64_t)(uintptr_t)&main;
    out->stack_addr = (uint64_t)(uintptr_t)&local_var;

    pthread_attr_t attr;
    if (pthread_getattr_np(pthread_self(), &attr) == 0) {
        void *sbase = NULL;
        size_t ssize = 0;
        if (pthread_attr_getstack(&attr, &sbase, &ssize) == 0) {
            out->stack_base = (uint64_t)(uintptr_t)sbase;
            out->stack_size = (uint64_t)ssize;
        }
        pthread_attr_destroy(&attr);
    }

    void *heap_ptr = malloc(4096);
    if (heap_ptr == NULL) {
        fprintf(stderr, "aslr-probe: malloc failed\n");
        return 1;
    }
    ((volatile uint8_t *)heap_ptr)[0] = 1;
    out->malloc_addr = (uint64_t)(uintptr_t)heap_ptr;
    free(heap_ptr);

    void *map_ptr = mmap(
        NULL,
        4096,
        PROT_READ | PROT_WRITE,
        MAP_PRIVATE | MAP_ANONYMOUS,
        -1,
        0
    );
    if (map_ptr == MAP_FAILED) {
        fprintf(stderr, "aslr-probe: mmap failed: %s\n", strerror(errno));
        return 1;
    }
    ((volatile uint8_t *)map_ptr)[0] = 2;
    out->mmap_addr = (uint64_t)(uintptr_t)map_ptr;
    munmap(map_ptr, 4096);

#ifdef WITH_DLOPEN
    void *handle = dlopen("/usr/lib/aslr-probe/libaslr-probe.so", RTLD_NOW);
    if (handle == NULL) {
        fprintf(stderr, "aslr-probe: dlopen failed: %s\n", dlerror());
        return 1;
    }
    typedef uintptr_t (*dso_fn_t)(void);
    dso_fn_t fn = (dso_fn_t)dlsym(handle, "aslr_dso_addr");
    if (fn == NULL) {
        fprintf(stderr, "aslr-probe: dlsym failed: %s\n", dlerror());
        dlclose(handle);
        return 1;
    }
    out->dso_addr = (uint64_t)fn();
    if (keep_dso && dso_handle_out != NULL) {
        *dso_handle_out = handle;
    } else {
        dlclose(handle);
    }
#else
    (void)keep_dso;
    (void)dso_handle_out;
#endif

    unsigned long aux_ptr = getauxval(AT_RANDOM);
    out->at_random_ptr = (uint64_t)aux_ptr;
    if (aux_ptr != 0) {
        uint64_t val = 0;
        memcpy(&val, (const void *)(uintptr_t)aux_ptr, sizeof(val));
        out->at_random_val = val;
    }
    return 0;
}

static void print_sample(const char *tag, const struct sample_data *s) {
    printf(
        "%s: main=0x%016" PRIx64 " stack=0x%016" PRIx64
        " stack_base=0x%016" PRIx64 " stack_size=0x%016" PRIx64
        " malloc=0x%016" PRIx64 " mmap=0x%016" PRIx64
        " dso=0x%016" PRIx64 " at_random_ptr=0x%016" PRIx64
        " at_random_val=0x%016" PRIx64 "\n",
        tag,
        s->main_addr,
        s->stack_addr,
        s->stack_base,
        s->stack_size,
        s->malloc_addr,
        s->mmap_addr,
        s->dso_addr,
        s->at_random_ptr,
        s->at_random_val
    );
}

static int wait_child_ok(pid_t pid) {
    int status = 0;
    pid_t waited = -1;
    for (int i = 0; i < WAIT_RETRIES; i++) {
        waited = waitpid(pid, &status, 0);
        if (waited >= 0 || errno != EINTR) {
            break;
        }
    }
    if (waited < 0) {
        fprintf(stderr, "aslr-probe: waitpid failed: %s\n", strerror(errno));
        return 1;
    }
    if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) {
        fprintf(stderr, "aslr-probe: child exited abnormally status=0x%x\n", status);
        return 1;
    }
    return 0;
}

static int cmp_u64(const void *a, const void *b) {
    uint64_t va = *(const uint64_t *)a;
    uint64_t vb = *(const uint64_t *)b;
    if (va < vb) {
        return -1;
    }
    if (va > vb) {
        return 1;
    }
    return 0;
}

static uint64_t monotonic_us(void) {
    struct timespec ts;
    if (clock_gettime(CLOCK_MONOTONIC, &ts) != 0) {
        return 0;
    }
    return (uint64_t)ts.tv_sec * 1000000ULL + (uint64_t)(ts.tv_nsec / 1000L);
}

static int run_bench(const char *binary, int runs) {
    if (runs <= 0 || runs > MAX_BENCH_RUNS) {
        fprintf(stderr, "aslr-probe: runs must be in 1..%d\n", MAX_BENCH_RUNS);
        return 1;
    }
    uint64_t samples[MAX_BENCH_RUNS];
    char *child_argv[2];
    child_argv[0] = (char *)binary;
    child_argv[1] = NULL;

    for (int i = 0; i < BENCH_WARMUP_RUNS; i++) {
        pid_t pid = 0;
        int rc = posix_spawn(&pid, binary, NULL, NULL, child_argv, environ);
        if (rc != 0) {
            fprintf(stderr, "aslr-probe: posix_spawn warmup failed: %s\n", strerror(rc));
            return 1;
        }
        if (wait_child_ok(pid) != 0) {
            return 1;
        }
    }

    for (int i = 0; i < runs; i++) {
        uint64_t t0 = monotonic_us();
        pid_t pid = 0;
        int rc = posix_spawn(&pid, binary, NULL, NULL, child_argv, environ);
        if (rc != 0) {
            fprintf(stderr, "aslr-probe: posix_spawn failed: %s\n", strerror(rc));
            return 1;
        }
        if (wait_child_ok(pid) != 0) {
            return 1;
        }
        uint64_t t1 = monotonic_us();
        samples[i] = t1 >= t0 ? (t1 - t0) : 0;
    }

    qsort(samples, (size_t)runs, sizeof(uint64_t), cmp_u64);
    uint64_t min_us = samples[0];
    uint64_t median_us = samples[runs / 2];
    int p95_idx = (runs * 95 + 99) / 100 - 1;
    if (p95_idx < 0) {
        p95_idx = 0;
    }
    if (p95_idx >= runs) {
        p95_idx = runs - 1;
    }
    uint64_t p95_us = samples[p95_idx];
    int p99_idx = (runs * 99 + 99) / 100 - 1;
    if (p99_idx < 0) {
        p99_idx = 0;
    }
    if (p99_idx >= runs) {
        p99_idx = runs - 1;
    }
    uint64_t p99_us = samples[p99_idx];
    uint64_t max_us = samples[runs - 1];
    printf(
        "aslr-bench: bin=%s runs=%d min_us=%" PRIu64 " median_us=%" PRIu64
        " p95_us=%" PRIu64 " p99_us=%" PRIu64 " max_us=%" PRIu64 "\n",
        binary,
        runs,
        min_us,
        median_us,
        p95_us,
        p99_us,
        max_us
    );
    return 0;
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "sample";

    if (strcmp(mode, "sample") == 0) {
        struct sample_data s;
        if (collect_sample(&s, 0, NULL) != 0) {
            return 1;
        }
        print_sample("aslr-probe", &s);
        return 0;
    }

    if (strcmp(mode, "reexec") == 0) {
        struct sample_data s;
        if (collect_sample(&s, 0, NULL) != 0) {
            return 1;
        }
        print_sample("aslr-probe-parent", &s);
        fflush(stdout);
        char *next_argv[3];
        next_argv[0] = argv[0];
        next_argv[1] = "reexec-child";
        next_argv[2] = NULL;
        execv(argv[0], next_argv);
        fprintf(stderr, "aslr-probe: execv failed: %s\n", strerror(errno));
        return 127;
    }

    if (strcmp(mode, "reexec-child") == 0) {
        struct sample_data s;
        if (collect_sample(&s, 0, NULL) != 0) {
            return 1;
        }
        print_sample("aslr-probe-reexec", &s);
        return 0;
    }

    if (strcmp(mode, "fork") == 0) {
        struct sample_data parent;
        void *dso_handle = NULL;
        if (collect_sample(&parent, 1, &dso_handle) != 0) {
            return 1;
        }
        print_sample("aslr-probe-fork-parent", &parent);
        fflush(stdout);
        pid_t pid = fork();
        if (pid < 0) {
            fprintf(stderr, "aslr-probe: fork failed: %s\n", strerror(errno));
            return 1;
        }
        if (pid == 0) {
            struct sample_data child;
            memset(&child, 0, sizeof(child));
            volatile uint64_t child_local = 0;
            child.main_addr = (uint64_t)(uintptr_t)&main;
            child.stack_addr = (uint64_t)(uintptr_t)&child_local;
            pthread_attr_t attr;
            if (pthread_getattr_np(pthread_self(), &attr) == 0) {
                void *sbase = NULL;
                size_t ssize = 0;
                if (pthread_attr_getstack(&attr, &sbase, &ssize) == 0) {
                    child.stack_base = (uint64_t)(uintptr_t)sbase;
                    child.stack_size = (uint64_t)ssize;
                }
                pthread_attr_destroy(&attr);
            }
            void *map_ptr = mmap(
                NULL,
                4096,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0
            );
            if (map_ptr != MAP_FAILED) {
                child.mmap_addr = (uint64_t)(uintptr_t)map_ptr;
                munmap(map_ptr, 4096);
            }
#ifdef WITH_DLOPEN
            if (dso_handle != NULL) {
                typedef uintptr_t (*dso_fn_t)(void);
                dso_fn_t fn = (dso_fn_t)dlsym(dso_handle, "aslr_dso_addr");
                if (fn != NULL) {
                    child.dso_addr = (uint64_t)fn();
                }
            }
#endif
            unsigned long aux_ptr = getauxval(AT_RANDOM);
            child.at_random_ptr = (uint64_t)aux_ptr;
            if (aux_ptr != 0) {
                uint64_t val = 0;
                memcpy(&val, (const void *)(uintptr_t)aux_ptr, sizeof(val));
                child.at_random_val = val;
            }
            print_sample("aslr-probe-fork-child", &child);
            fflush(stdout);
            _exit(0);
        }
#ifdef WITH_DLOPEN
        if (dso_handle != NULL) {
            dlclose(dso_handle);
        }
#endif
        return wait_child_ok(pid);
    }

    if (strcmp(mode, "bench") == 0) {
        if (argc < 4) {
            fprintf(stderr, "usage: %s bench <binary> <runs>\n", argv[0]);
            return 2;
        }
        int runs = atoi(argv[3]);
        return run_bench(argv[2], runs);
    }

    fprintf(stderr, "aslr-probe: unknown mode %s\n", mode);
    return 2;
}
