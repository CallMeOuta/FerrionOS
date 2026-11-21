#include <dlfcn.h>
#include <stdio.h>

int dlopen_ctor_leaf_finalized;

/* Resolved and called by the constructor through the real loader API. */
int dlopen_ctor_host_value(void)
{
    return 41;
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("dlopen-ctor: begin outer-dlopen");
    void *outer = dlopen("/usr/lib/libdlopen-ctor-outer.so", RTLD_NOW | RTLD_LOCAL);
    if (!outer) {
        printf("dlopen-ctor: FAIL outer-dlopen: %s\n", dlerror());
        return 1;
    }
    int (*outer_value)(void) = (int (*)(void))dlsym(outer, "dlopen_ctor_outer_value");
    int ready = outer_value && outer_value() == 91;
    if (!ready)
        puts("dlopen-ctor: FAIL outer-initialized-symbol");
    else
        puts("dlopen-ctor: pass outer-initialized-symbol");
    if (dlclose(outer) != 0) {
        puts("dlopen-ctor: FAIL outer-dlclose");
        return 1;
    }
    puts("dlopen-ctor: pass outer-dlclose");
    if (!ready)
        return 1;
    puts("dlopen-ctor: PASS");
    return 0;
}
