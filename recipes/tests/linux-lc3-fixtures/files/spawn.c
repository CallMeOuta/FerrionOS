/* posix_spawn of a missing file must report ENOENT through the memory the child shares with
   the parent; a spawn of /bin/true must run it. */
#include <errno.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
extern char **environ;
int main(void) {
    pid_t pid;
    char *missing[] = {"/no/such/program", 0};
    int r = posix_spawn(&pid, missing[0], 0, 0, missing, environ);
    printf("spawn missing: %s\n", r == ENOENT ? "ENOENT" : strerror(r));
    char *yes[] = {"/bin/true", 0};
    int status = -1;
    r = posix_spawn(&pid, yes[0], 0, 0, yes, environ);
    if (r == 0) waitpid(pid, &status, 0);
    printf("spawn true: %d, status %d\n", r, WIFEXITED(status) ? WEXITSTATUS(status) : -1);
    return 0;
}
