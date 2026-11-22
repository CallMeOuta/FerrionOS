#include <stdint.h>
#include <stdio.h>
#include <string.h>

static int static_target = 42;
int *const static_relro_ptr __attribute__((section(".data.rel.ro"))) = &static_target;
static volatile int static_mutable = 1;

int main(int argc, char **argv)
{
    if (argc != 2) {
        fputs("Usage: relro-probe-static info|write-relro\n", stderr);
        return 2;
    }

    if (strcmp(argv[1], "info") == 0) {
        static_mutable = 99;
        printf("STATIC_RELRO_INFO value=%d mutable=%d\n", *static_relro_ptr, (int)static_mutable);
        fflush(stdout);
        return 0;
    }

    if (strcmp(argv[1], "write-relro") == 0) {
        if (*static_relro_ptr != 42) {
            fputs("RELRO_ERROR unexpected_static_value\n", stderr);
            return 4;
        }
        puts("RELRO_BEFORE_WRITE");
        fflush(stdout);
        *(volatile uintptr_t *)(uintptr_t)&static_relro_ptr = 0x1234;
        puts("RELRO_WRITE_SUCCEEDED");
        fflush(stdout);
        return 0;
    }

    fputs("RELRO_ERROR unknown_mode\n", stderr);
    return 2;
}
