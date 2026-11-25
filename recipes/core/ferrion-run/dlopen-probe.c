#include <dlfcn.h>
#include <stdio.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: ferrion-dlopen-probe PATH\n");
        return 2;
    }
    void *handle = dlopen(argv[1], RTLD_NOW);
    if (!handle) {
        const char *err = dlerror();
        printf("DLOPEN ERR %s\n", err ? err : "unknown");
        return 1;
    }
    dlclose(handle);
    printf("DLOPEN OK\n");
    return 0;
}
