#include <dlfcn.h>

static int initialized;

__attribute__((constructor)) static void initialize(void)
{
    initialized = 73;
}

__attribute__((destructor)) static void finalize(void)
{
    int *count = dlsym(RTLD_DEFAULT, "dlopen_ctor_leaf_finalized");
    if (count)
        ++*count;
}

int dlopen_ctor_leaf_value(void)
{
    return initialized;
}
