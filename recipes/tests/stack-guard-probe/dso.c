#include <stdint.h>
#include <stdlib.h>

static volatile uintptr_t dso_ctor_guard_value = 0;

__attribute__((noinline))
static void dso_poke(volatile uint64_t *p, uint64_t v) {
    p[0] = v;
    p[1] = v;
    p[2] = v;
    p[3] = v;
    p[4] = v;
    p[5] = v;
}

__attribute__((constructor))
static void dso_ctor(void) {
    uint64_t buf[4];
    volatile uint64_t *p = buf;
    p[0] = 1;
    p[3] = 2;
    dso_ctor_guard_value = __stack_chk_guard;
}

uintptr_t dso_get_ctor_guard(void) {
    return dso_ctor_guard_value;
}

uintptr_t dso_get_live_guard(void) {
    volatile uintptr_t g = __stack_chk_guard;
    return g;
}

__attribute__((noinline))
int dso_smash_with(uint64_t val) {
    uint64_t buf[4];
    dso_poke(buf, val);
    return (int)buf[0];
}
