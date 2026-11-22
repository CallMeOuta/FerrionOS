#ifndef _RELIBC_BITS_FORTIFY_STRING_H
#define _RELIBC_BITS_FORTIFY_STRING_H

#if defined(__USE_FORTIFY_LEVEL) && __USE_FORTIFY_LEVEL > 0

#ifdef __cplusplus
extern "C" {
#endif

void *__memcpy_chk(void *dest, const void *src, size_t len, size_t destlen);
void *__memmove_chk(void *dest, const void *src, size_t len, size_t destlen);
void *__memset_chk(void *dest, int c, size_t len, size_t destlen);
char *__strcpy_chk(char *dest, const char *src, size_t destlen);
char *__stpcpy_chk(char *dest, const char *src, size_t destlen);
char *__strncpy_chk(char *dest, const char *src, size_t len, size_t destlen);
char *__stpncpy_chk(char *dest, const char *src, size_t len, size_t destlen);
char *__strcat_chk(char *dest, const char *src, size_t destlen);
char *__strncat_chk(char *dest, const char *src, size_t n, size_t destlen);

__ferrion_fortify_inline void *memcpy(void *s1, const void *s2, size_t n) {
    return __builtin___memcpy_chk(s1, s2, n, __ferrion_bos0(s1));
}

__ferrion_fortify_inline void *memmove(void *s1, const void *s2, size_t n) {
    return __builtin___memmove_chk(s1, s2, n, __ferrion_bos0(s1));
}

__ferrion_fortify_inline void *memset(void *s, int c, size_t n) {
    return __builtin___memset_chk(s, c, n, __ferrion_bos0(s));
}

__ferrion_fortify_inline char *strcpy(char *dst, const char *src) {
    return __builtin___strcpy_chk(dst, src, __ferrion_bos(dst));
}

__ferrion_fortify_inline char *stpcpy(char *s1, const char *s2) {
    return __builtin___stpcpy_chk(s1, s2, __ferrion_bos(s1));
}

__ferrion_fortify_inline char *strncpy(char *s1, const char *s2, size_t n) {
    return __builtin___strncpy_chk(s1, s2, n, __ferrion_bos(s1));
}

__ferrion_fortify_inline char *stpncpy(char *s1, const char *s2, size_t n) {
    return __builtin___stpncpy_chk(s1, s2, n, __ferrion_bos(s1));
}

__ferrion_fortify_inline char *strcat(char *s1, const char *s2) {
    return __builtin___strcat_chk(s1, s2, __ferrion_bos(s1));
}

__ferrion_fortify_inline char *strncat(char *s1, const char *s2, size_t n) {
    return __builtin___strncat_chk(s1, s2, n, __ferrion_bos(s1));
}

#ifdef __cplusplus
}
#endif

#endif

#endif
