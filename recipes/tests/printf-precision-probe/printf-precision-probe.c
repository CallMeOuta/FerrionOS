#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <wchar.h>

static void emit_double(const char *name, const char *fmt, double val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

static void emit_star_double(const char *name, const char *fmt, int prec, double val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, prec, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

static void emit_int(const char *name, const char *fmt, int val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

static void emit_uint(const char *name, const char *fmt, unsigned int val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

static void emit_str(const char *name, const char *fmt, const char *val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

static void emit_star_str(const char *name, const char *fmt, int prec, const char *val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, prec, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

static void emit_wstr(const char *name, const char *fmt, const wchar_t *val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

static void emit_invalid(const char *name, const char *fmt) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("case %s: ret=%d out=[%s]\n", name, n, buf);
}

int main(void) {
    emit_double("dot-f-2.5", "%.f", 2.5);
    emit_double("dot-f-3.5", "%.f", 3.5);
    emit_double("dot-f-neg2.5", "%.f", -2.5);
    emit_double("dot-f-0.0", "%.f", 0.0);
    emit_double("dot0-f-2.5", "%.0f", 2.5);
    emit_double("dot0-f-3.5", "%.0f", 3.5);
    emit_double("alt-dot-f-2.5", "%#.f", 2.5);
    emit_double("alt-dot-f-0.0", "%#.f", 0.0);
    emit_int("dot-d-42", "%.d", 42);
    emit_int("dot-d-0", "%.d", 0);
    emit_int("dot-d-neg5", "%.d", -5);
    emit_int("dot0-d-0", "%.0d", 0);
    emit_uint("dot-u-0", "%.u", 0u);
    emit_uint("dot-u-42", "%.u", 42u);
    emit_uint("dot-x-0", "%.x", 0u);
    emit_uint("dot-x-2a", "%.x", 0x2au);
    emit_str("dot-s-hello", "%.s", "hello");
    emit_str("dot-s-empty", "%.s", "");
    emit_str("w5-dot-s-hello", "%5.s", "hello");
    emit_double("dot-e-2.5", "%.e", 2.5);
    emit_double("dot-e-0.0", "%.e", 0.0);
    emit_double("alt-dot-e-2.5", "%#.e", 2.5);
    emit_double("dot-g-2.5", "%.g", 2.5);
    emit_double("dot-g-1234", "%.g", 1234.0);
    emit_double("dot-g-0.0", "%.g", 0.0);
    emit_double("dot0-g-2.5", "%.0g", 2.5);
    emit_double("dot0-g-0.0", "%.0g", 0.0);
    emit_double("alt-dot-g-2.5", "%#.g", 2.5);
    emit_double("alt-dot-g-0.0", "%#.g", 0.0);
    emit_star_double("star-f-0-2.5", "%.*f", 0, 2.5);
    emit_invalid("invalid-bare-end", "%.");
    emit_invalid("invalid-bad-conv", "%.z");
    emit_str("dot-s-null", "%.s", NULL);
    emit_str("dot0-s-null", "%.0s", NULL);
    emit_str("dot1-s-null", "%.1s", NULL);
    emit_str("dot2-s-null", "%.2s", NULL);
    emit_str("dot3-s-null", "%.3s", NULL);
    emit_str("dot4-s-null", "%.4s", NULL);
    emit_str("dot5-s-null", "%.5s", NULL);
    emit_str("dot6-s-null", "%.6s", NULL);
    emit_str("dot7-s-null", "%.7s", NULL);
    emit_str("s-null", "%s", NULL);
    emit_star_str("star-s-0-null", "%.*s", 0, NULL);
    emit_star_str("star-s-5-null", "%.*s", 5, NULL);
    emit_star_str("star-s-6-null", "%.*s", 6, NULL);
    emit_star_str("star-s-neg1-null", "%.*s", -1, NULL);
    emit_str("w8-dot0-s-null", "%8.0s", NULL);
    emit_str("w8-dot3-s-null", "%8.3s", NULL);
    emit_str("lw8-dot3-s-null", "%-8.3s", NULL);
    emit_str("w8-dot5-s-null", "%8.5s", NULL);
    emit_str("w8-dot6-s-null", "%8.6s", NULL);
    emit_str("lw8-dot6-s-null", "%-8.6s", NULL);
    emit_str("w8-s-null", "%8s", NULL);
    emit_str("lw8-s-null", "%-8s", NULL);
    emit_str("w4-dot6-s-null", "%4.6s", NULL);
    emit_wstr("ls-null", "%ls", NULL);
    emit_wstr("dot0-ls-null", "%.0ls", NULL);
    emit_wstr("dot5-ls-null", "%.5ls", NULL);
    emit_wstr("dot6-ls-null", "%.6ls", NULL);
    emit_wstr("w8-dot3-ls-null", "%8.3ls", NULL);
    emit_wstr("w8-dot6-ls-null", "%8.6ls", NULL);
    printf("printf-precision-probe: DONE\n");
    return 0;
}
