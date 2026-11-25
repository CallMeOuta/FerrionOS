#include <stdint.h>

uintptr_t aslr_dso_addr(void) {
    return (uintptr_t)&aslr_dso_addr;
}
