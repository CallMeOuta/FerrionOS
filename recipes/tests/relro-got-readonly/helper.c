#include <stdint.h>

int relro_helper_data = 42;

int relro_helper_value(void)
{
    return relro_helper_data;
}

uintptr_t relro_helper_self_addr(void)
{
    return (uintptr_t)&relro_helper_value;
}

int *const relro_helper_const_ptr = &relro_helper_data;

uintptr_t relro_helper_relro_slot(void)
{
    return (uintptr_t)&relro_helper_const_ptr;
}
