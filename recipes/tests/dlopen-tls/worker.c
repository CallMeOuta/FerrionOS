#include <stdio.h>

__thread unsigned long *dlopen_tls_pointer;

void *dlopen_tls_worker(void *argument)
{
    (void)argument;
    if (dlopen_tls_pointer) {
        __sync_fetch_and_add(dlopen_tls_pointer, 1);
        printf("dlopen-tls: FAIL thread-tls-initial (%p)\n", (void *)dlopen_tls_pointer);
        return (void *)1;
    }
    puts("dlopen-tls: pass thread-tls-initial");

    unsigned long word;
    dlopen_tls_pointer = &word;
    *dlopen_tls_pointer = 0x8877665544332211UL;
    if (*dlopen_tls_pointer != 0x8877665544332211UL) {
        printf("dlopen-tls: FAIL thread-tls-write (%#lx)\n", *dlopen_tls_pointer);
        return (void *)1;
    }
    puts("dlopen-tls: pass thread-tls-write");
    return NULL;
}
