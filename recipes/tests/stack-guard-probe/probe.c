#include <elf.h>
#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <sys/wait.h>
#include <unistd.h>

#ifdef WITH_DSO
extern uintptr_t dso_get_ctor_guard(void);
extern uintptr_t dso_get_live_guard(void);
extern int dso_smash_with(uint64_t val);
#endif

static volatile uintptr_t exe_ctor_guard_value = 0;

__attribute__((constructor))
static void exe_ctor(void) {
    uint64_t buf[4];
    volatile uint64_t *p = buf;
    p[0] = 3;
    p[3] = 4;
    exe_ctor_guard_value = __stack_chk_guard;
}

__attribute__((noinline))
static void local_poke(volatile uint64_t *p, uint64_t v) {
    p[0] = v;
    p[1] = v;
    p[2] = v;
    p[3] = v;
    p[4] = v;
    p[5] = v;
}

__attribute__((noinline))
static int local_smash_with(uint64_t val) {
    uint64_t buf[4];
    local_poke(buf, val);
    return (int)buf[0];
}

static uint64_t read_aux_random_word(void) {
    unsigned long ptr = getauxval(AT_RANDOM);
    if (ptr == 0) {
        return 0;
    }
    uint64_t val = 0;
    memcpy(&val, (const void *)ptr, sizeof(val));
    return val;
}

static int run_smash_in_child(const char *label, int use_dso, uint64_t fill) {
    fflush(stdout);
    pid_t pid = fork();
    if (pid < 0) {
        fprintf(stderr, "stack-guard-probe: fork failed: %s\n", strerror(errno));
        return 1;
    }
    if (pid == 0) {
        int r = 0;
#ifdef WITH_DSO
        if (use_dso) {
            r = dso_smash_with(fill);
        } else {
            r = local_smash_with(fill);
        }
#else
        (void)use_dso;
        r = local_smash_with(fill);
#endif
        printf("stack-guard-probe: %s SURVIVED %d\n", label, r);
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
        fprintf(stderr, "stack-guard-probe: waitpid failed: %s\n", strerror(errno));
        return 1;
    }
    if (WIFSIGNALED(status)) {
        printf("stack-guard-probe: %s ABORTED sig=%d\n", label, WTERMSIG(status));
        return 0;
    }
    if (WIFEXITED(status)) {
        printf("stack-guard-probe: %s EXITED code=%d\n", label, WEXITSTATUS(status));
        return 1;
    }
    printf("stack-guard-probe: %s OTHER status=%d\n", label, status);
    return 1;
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "guard";
    volatile uintptr_t guard = __stack_chk_guard;
    uint64_t aux_word = read_aux_random_word();

    if (strcmp(mode, "guard") == 0) {
#ifdef WITH_DSO
        printf(
            "stack-guard-probe: guard=0x%016" PRIx64 " exe_ctor=0x%016" PRIx64
            " dso_ctor=0x%016" PRIx64 " dso_live=0x%016" PRIx64 " aux=0x%016" PRIx64 "\n",
            (uint64_t)guard,
            (uint64_t)exe_ctor_guard_value,
            (uint64_t)dso_get_ctor_guard(),
            (uint64_t)dso_get_live_guard(),
            aux_word
        );
#else
        printf(
            "stack-guard-probe: guard=0x%016" PRIx64 " exe_ctor=0x%016" PRIx64
            " aux=0x%016" PRIx64 "\n",
            (uint64_t)guard,
            (uint64_t)exe_ctor_guard_value,
            aux_word
        );
#endif
        return 0;
    }

    if (strcmp(mode, "child") == 0) {
        printf("stack-guard-probe: child=0x%016" PRIx64 " aux=0x%016" PRIx64 "\n",
               (uint64_t)guard, aux_word);
        return 0;
    }

    if (strcmp(mode, "exec") == 0) {
        printf("stack-guard-probe: parent=0x%016" PRIx64 " aux=0x%016" PRIx64 "\n",
               (uint64_t)guard, aux_word);
        fflush(stdout);
        pid_t pid = fork();
        if (pid < 0) {
            fprintf(stderr, "stack-guard-probe: fork failed: %s\n", strerror(errno));
            return 1;
        }
        if (pid == 0) {
            char *child_argv[3];
            child_argv[0] = argv[0];
            child_argv[1] = "child";
            child_argv[2] = NULL;
            execvp(argv[0], child_argv);
            fprintf(stderr, "stack-guard-probe: execvp failed: %s\n", strerror(errno));
            _exit(127);
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
            return 1;
        }
        return (WIFEXITED(status) && WEXITSTATUS(status) == 0) ? 0 : 1;
    }

    if (strcmp(mode, "smash") == 0) {
        int rc = run_smash_in_child("smash-pattern", 0, UINT64_C(0x4141414141414141));
        int rc2 = run_smash_in_child("smash-const", 0, UINT64_C(0xd048c37519fcadfe));
#ifdef WITH_DSO
        int rc3 = run_smash_in_child("dso-smash-pattern", 1, UINT64_C(0x4141414141414141));
        int rc4 = run_smash_in_child("dso-smash-const", 1, UINT64_C(0xd048c37519fcadfe));
        return (rc == 0 && rc2 == 0 && rc3 == 0 && rc4 == 0) ? 0 : 1;
#else
        return (rc == 0 && rc2 == 0) ? 0 : 1;
#endif
    }

    fprintf(stderr, "stack-guard-probe: unknown mode %s\n", mode);
    return 2;
}
