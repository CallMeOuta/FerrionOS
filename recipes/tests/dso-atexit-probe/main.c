#include <dlfcn.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv)
{
    if (argc != 2 || (strcmp(argv[1], "close") && strcmp(argv[1], "keep-open"))) {
        fputs("Usage: dso-atexit-probe close|keep-open\n", stderr);
        return 2;
    }
    void *object = dlopen("/usr/lib/dso-atexit-probe/libdso-atexit-callback.so", RTLD_NOW | RTLD_LOCAL);
    if (!object) {
        fprintf(stderr, "DSO_ATEXIT_ERROR dlopen: %s\n", dlerror());
        return 1;
    }
    int (*register_callback)(void) = dlsym(object, "register_callback");
    if (!register_callback || register_callback()) {
        fputs("DSO_ATEXIT_ERROR registration\n", stderr);
        return 1;
    }
    if (!strcmp(argv[1], "close")) {
        if (dlclose(object)) {
            fprintf(stderr, "DSO_ATEXIT_ERROR dlclose: %s\n", dlerror());
            return 1;
        }
        puts("DSO_ATEXIT_CLOSED");
    }
    puts("DSO_ATEXIT_RETURN");
    fflush(stdout);
    return 0;
}
