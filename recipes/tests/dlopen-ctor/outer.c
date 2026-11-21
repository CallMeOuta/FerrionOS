#include <dlfcn.h>
#include <stdio.h>

static int initialized;

__attribute__((constructor)) static void initialize(void)
{
    puts("dlopen-ctor: begin constructor-dlsym");
    int (*host_value)(void) = (int (*)(void))dlsym(RTLD_DEFAULT, "dlopen_ctor_host_value");
    if (!host_value || host_value() != 41) {
        puts("dlopen-ctor: FAIL constructor-dlsym");
        return;
    }
    puts("dlopen-ctor: pass constructor-dlsym");

    void *leaf = dlopen("/usr/lib/libdlopen-ctor-leaf.so", RTLD_NOW | RTLD_LOCAL);
    if (!leaf) {
        puts("dlopen-ctor: FAIL nested-dlopen");
        return;
    }
    int (*leaf_value)(void) = (int (*)(void))dlsym(leaf, "dlopen_ctor_leaf_value");
    int ready = leaf_value && leaf_value() == 73;
    if (!ready)
        puts("dlopen-ctor: FAIL nested-initialized-symbol");
    else
        puts("dlopen-ctor: pass nested-initialized-symbol");
    if (dlclose(leaf) != 0) {
        puts("dlopen-ctor: FAIL nested-dlclose");
        return;
    }
    puts("dlopen-ctor: pass nested-dlclose");
    int *finalized = dlsym(RTLD_DEFAULT, "dlopen_ctor_leaf_finalized");
    if (!finalized || *finalized != 1) {
        puts("dlopen-ctor: FAIL nested-destructor");
        return;
    }
    puts("dlopen-ctor: pass nested-destructor");
    if (ready)
        initialized = 91;
}

int dlopen_ctor_outer_value(void)
{
    return initialized;
}
