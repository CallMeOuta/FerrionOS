#include <stddef.h>
#include <stdio.h>
#include <string.h>

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

static void emit_str(const char *prefix, const char *name, const char *fmt, const char *val) {
    char buf[64];
    int (*fn)(char *, size_t, const char *, ...) = snprintf;
    memset(buf, 0, sizeof(buf));
    int n = fn(buf, sizeof(buf), fmt, val);
    if (n < 0) {
        buf[0] = '\0';
    }
    printf("%s %s: ret=%d out=[%s]\n", prefix, name, n, buf);
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
    emit_str("case", "dot-s-hello", "%.s", "hello");
    emit_str("case", "dot-s-empty", "%.s", "");
    emit_str("case", "w5-dot-s-hello", "%5.s", "hello");
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
    emit_str("null-check", "dot-s-null", "%.s", NULL);
    emit_str("null-check", "dot0-s-null", "%.0s", NULL);
    printf("printf-precision-probe: DONE\n");
    return 0;
}
