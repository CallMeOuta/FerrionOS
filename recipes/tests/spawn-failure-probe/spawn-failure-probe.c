#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define MISSING_PROGRAM_CALLS 25
#define MISSING_ACTION_CALLS 25
#define PATH_CAP 128
#define SPAWN_FAILURE_HOLD_MS 4000
#define HOLD_STEP_MS 100
#define HOLD_MAX_STEPS (SPAWN_FAILURE_HOLD_MS / HOLD_STEP_MS)

extern char **environ;

int main(void)
{
    setvbuf(stdout, NULL, _IOLBF, 0);

    if (getuid() == 0 || geteuid() == 0) {
        fprintf(stdout, "spawn-failure-probe: fail must_run_unprivileged uid=%d euid=%d\n",
            (int)getuid(), (int)geteuid());
        return 1;
    }

    int prog_enoent = 0;
    for (int i = 0; i < MISSING_PROGRAM_CALLS; i++) {
        char path[PATH_CAP];
        int n = snprintf(path, sizeof(path), "/home/user/no-such-spawn-prog-07-%d", i);
        assert(n > 0 && (size_t)n < sizeof(path));
        char *argv[] = { path, NULL };
        pid_t child = -1;
        int rc = posix_spawn(&child, path, NULL, NULL, argv, environ);
        if (rc != ENOENT) {
            fprintf(stdout, "spawn-failure-probe: fail missing_program iter=%d rc=%d expected=%d child=%ld\n",
                i, rc, ENOENT, (long)child);
            return 1;
        }
        prog_enoent++;
    }

    int action_enoent = 0;
    for (int i = 0; i < MISSING_ACTION_CALLS; i++) {
        char path[PATH_CAP];
        int n = snprintf(path, sizeof(path), "/home/user/no-such-spawn-open-07-%d", i);
        assert(n > 0 && (size_t)n < sizeof(path));
        posix_spawn_file_actions_t fa;
        int irc = posix_spawn_file_actions_init(&fa);
        if (irc != 0) {
            fprintf(stdout, "spawn-failure-probe: fail fa_init iter=%d rc=%d\n", i, irc);
            return 1;
        }
        int arc = posix_spawn_file_actions_addopen(&fa, 3, path, O_RDONLY, 0);
        if (arc != 0) {
            (void)posix_spawn_file_actions_destroy(&fa);
            fprintf(stdout, "spawn-failure-probe: fail fa_addopen iter=%d rc=%d\n", i, arc);
            return 1;
        }
        char prog[] = "/usr/bin/true";
        char *argv[] = { prog, NULL };
        pid_t child = -1;
        int rc = posix_spawn(&child, prog, &fa, NULL, argv, environ);
        (void)posix_spawn_file_actions_destroy(&fa);
        if (rc != ENOENT) {
            fprintf(stdout, "spawn-failure-probe: fail missing_addopen iter=%d rc=%d expected=%d child=%ld\n",
                i, rc, ENOENT, (long)child);
            return 1;
        }
        action_enoent++;
    }

    int st = 0;
    errno = 0;
    pid_t w = waitpid(-1, &st, WNOHANG);
    if (w != -1 || errno != ECHILD) {
        fprintf(stdout, "spawn-failure-probe: fail waitpid w=%ld errno=%d expected=%d st=%d\n",
            (long)w, errno, ECHILD, st);
        return 1;
    }

    fprintf(stdout,
        "spawn-failure-probe: hold pid=%ld missing_program=%d/%d missing_addopen=%d/%d waitpid_echild=1\n",
        (long)getpid(), prog_enoent, MISSING_PROGRAM_CALLS, action_enoent, MISSING_ACTION_CALLS);
    fflush(stdout);

    struct timespec step_ts = { 0, HOLD_STEP_MS * 1000000L };
    for (int s = 0; s < HOLD_MAX_STEPS; s++) {
        (void)nanosleep(&step_ts, NULL);
    }

    fprintf(stdout, "spawn-failure-probe: PASS\n");
    return 0;
}
