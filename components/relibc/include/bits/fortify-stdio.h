#ifndef _RELIBC_BITS_FORTIFY_STDIO_H
#define _RELIBC_BITS_FORTIFY_STDIO_H

#if defined(__USE_FORTIFY_LEVEL) && __USE_FORTIFY_LEVEL > 0

#ifdef __cplusplus
extern "C" {
#endif

int __vsnprintf_chk(char *s, size_t maxlen, int flag, size_t slen, const char *format, va_list ap);
int __snprintf_chk(char *s, size_t maxlen, int flag, size_t slen, const char *format, ...);
int __vsprintf_chk(char *s, int flag, size_t slen, const char *format, va_list ap);
int __sprintf_chk(char *s, int flag, size_t slen, const char *format, ...);
char *__fgets_chk(char *s, size_t buflen, int n, FILE *stream);
char *__ferrion_fgets_alias(char *original, int max, FILE *stream) __asm__("fgets");

__ferrion_fortify_inline int vsnprintf(char *s, size_t n, const char *format, va_list ap) {
    return __builtin___vsnprintf_chk(s, n, __USE_FORTIFY_LEVEL - 1, __ferrion_bos(s), format, ap);
}

__ferrion_fortify_inline int vsprintf(char *s, const char *format, va_list ap) {
    return __builtin___vsprintf_chk(s, __USE_FORTIFY_LEVEL - 1, __ferrion_bos(s), format, ap);
}

#if defined(__GNUC__) && !defined(__clang__)
__ferrion_fortify_inline int snprintf(char *s, size_t n, const char *format, ...) {
    return __builtin___snprintf_chk(s, n, __USE_FORTIFY_LEVEL - 1, __ferrion_bos(s), format, __builtin_va_arg_pack());
}

__ferrion_fortify_inline int sprintf(char *s, const char *format, ...) {
    return __builtin___sprintf_chk(s, __USE_FORTIFY_LEVEL - 1, __ferrion_bos(s), format, __builtin_va_arg_pack());
}
#else
#define snprintf(s, n, ...) __builtin___snprintf_chk((s), (n), __USE_FORTIFY_LEVEL - 1, __ferrion_bos(s), __VA_ARGS__)
#define sprintf(s, ...) __builtin___sprintf_chk((s), __USE_FORTIFY_LEVEL - 1, __ferrion_bos(s), __VA_ARGS__)
#endif

__ferrion_fortify_inline char *fgets(char *original, int max, FILE *stream) {
    size_t __sz = __ferrion_bos(original);
    if (__sz != (size_t)-1) {
        return __fgets_chk(original, __sz, max, stream);
    }
    return __ferrion_fgets_alias(original, max, stream);
}

#ifdef __cplusplus
}
#endif

#endif

#endif
