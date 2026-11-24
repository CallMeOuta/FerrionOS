#include <math.h>
#include <stdio.h>

static void emit_d(const char *name, double val) {
    printf("double %s %.17g\n", name, val);
}

static void emit_f(const char *name, float val) {
    printf("float %s %.9g\n", name, (double)val);
}

static void emit_dr(const char *name, double val, int sign) {
    printf("double_sign %s %.17g %d\n", name, val, sign);
}

static void emit_fr(const char *name, float val, int sign) {
    printf("float_sign %s %.9g %d\n", name, (double)val, sign);
}

int main(void) {
    const double xs[] = {0.5, 1.0, 2.5, 10.0};
    const int nxs = (int)(sizeof(xs) / sizeof(xs[0]));
    for (int i = 0; i < nxs; i++) {
        double x = xs[i];
        float xf = (float)x;
        char label[64];

        snprintf(label, sizeof(label), "j0(%.1f)", x);
        emit_d(label, j0(x));
        snprintf(label, sizeof(label), "j1(%.1f)", x);
        emit_d(label, j1(x));
        snprintf(label, sizeof(label), "jn(2,%.1f)", x);
        emit_d(label, jn(2, x));
        snprintf(label, sizeof(label), "y0(%.1f)", x);
        emit_d(label, y0(x));
        snprintf(label, sizeof(label), "y1(%.1f)", x);
        emit_d(label, y1(x));
        snprintf(label, sizeof(label), "yn(2,%.1f)", x);
        emit_d(label, yn(2, x));

        snprintf(label, sizeof(label), "j0f(%.1f)", x);
        emit_f(label, j0f(xf));
        snprintf(label, sizeof(label), "j1f(%.1f)", x);
        emit_f(label, j1f(xf));
        snprintf(label, sizeof(label), "jnf(2,%.1f)", x);
        emit_f(label, jnf(2, xf));
        snprintf(label, sizeof(label), "y0f(%.1f)", x);
        emit_f(label, y0f(xf));
        snprintf(label, sizeof(label), "y1f(%.1f)", x);
        emit_f(label, y1f(xf));
        snprintf(label, sizeof(label), "ynf(2,%.1f)", x);
        emit_f(label, ynf(2, xf));

        snprintf(label, sizeof(label), "tgamma(%.1f)", x);
        emit_d(label, tgamma(x));
        snprintf(label, sizeof(label), "tgammaf(%.1f)", x);
        emit_f(label, tgammaf(xf));
        snprintf(label, sizeof(label), "lgamma(%.1f)", x);
        emit_d(label, lgamma(x));
        snprintf(label, sizeof(label), "lgammaf(%.1f)", x);
        emit_f(label, lgammaf(xf));
    }

    emit_d("j0(0.0)", j0(0.0));
    emit_d("j1(0.0)", j1(0.0));

    const double gxs[] = {-1.5, -0.5, 0.5, 3.5};
    const int ngxs = (int)(sizeof(gxs) / sizeof(gxs[0]));
    for (int i = 0; i < ngxs; i++) {
        double x = gxs[i];
        float xf = (float)x;
        long double xl = (long double)x;
        int sd = 0;
        int sf = 0;
        int sl = 0;
        double vd = lgamma_r(x, &sd);
        float vf = lgammaf_r(xf, &sf);
        long double vl = lgammal_r(xl, &sl);
        char label[64];

        snprintf(label, sizeof(label), "lgamma_r(%.1f)", x);
        emit_dr(label, vd, sd);
        snprintf(label, sizeof(label), "lgammaf_r(%.1f)", x);
        emit_fr(label, vf, sf);
        snprintf(label, sizeof(label), "lgammal_r(%.1f)", x);
        emit_dr(label, (double)vl, sl);
    }

    printf("math-bessel-probe: DONE\n");
    return 0;
}
