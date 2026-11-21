#include <stdio.h>
#include <stdlib.h>

static void on_exit(void)
{
    puts("DSO_ATEXIT_CALLBACK");
    fflush(stdout);
}

int register_callback(void)
{
    printf("DSO_ATEXIT_ADDRESS=%p object=libdso-atexit-callback.so\n", (void *)on_exit);
    fflush(stdout);
    return atexit(on_exit);
}
