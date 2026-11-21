#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include "recipes/tests/gpu-probe/version.h"

#define EGL_BAD_NATIVE_WINDOW 1
#define EGL_BAD_ALLOC 2
#define EGL_BAD_MATCH 3
#define EGL_BAD_PARAMETER 4
#define EGL_WINDOW_BIT 1
#define __DRI_SWRAST_IMAGE_OP_SWAP 1
#define _EGL_WARNING 1

typedef int EGLint;
typedef struct { int Width, Height, GLColorspace; } _EGLSurface;
typedef struct { int unused; } _EGLDisplay;
typedef struct { int unused; } _EGLConfig;
struct dri2_egl_display { int unused; };
struct dri2_egl_config { int unused; };
struct dri_config { int unused; };
struct dri_drawable { int unused; };
struct dri2_egl_surface {
    _EGLSurface base;
    void *orb_window, *orb_surface;
};
static int last_error, borrowed_fd;
static unsigned char destination[64];
static void _eglError(int error, const char *message) { last_error = error; }
static void _eglLog(int level, const char *message) { assert(0); }
static struct dri2_egl_display *dri2_egl_display(_EGLDisplay *d) { return NULL; }
static struct dri2_egl_config *dri2_egl_config(_EGLConfig *c) { return NULL; }
static bool dri2_init_surface(_EGLSurface *s, _EGLDisplay *d, int type,
                              _EGLConfig *c, const EGLint *attrs, bool window,
                              void *native) { return true; }
static void *orb_window_from_fd(int fd) { borrowed_fd = fd; return NULL; }
static int orb_window_width(void *w) { assert(w); return 4; }
static int orb_window_height(void *w) { assert(w); return 4; }
static void orb_window_destroy(void *w) { assert(w); }
static void *orb_window_data(void *w) { return destination; }
static void *orb_surface_data(void *s) { return destination; }
static int orb_surface_width(void *s) { return 4; }
static int orb_surface_height(void *s) { return 4; }
static const struct dri_config *dri2_get_dri_config(struct dri2_egl_config *c,
                                                   int type, int color) { return NULL; }
static bool dri2_create_drawable(struct dri2_egl_display *d, const struct dri_config *c,
                                 struct dri2_egl_surface *s, void *loader) { return false; }

#include "orbital-functions.h"

int main(void)
{
    assert(mesa_version_matches("Mesa 26.3.1"));
    assert(mesa_version_matches("4.6 (Compatibility Profile) Mesa 26.3.1"));
    assert(mesa_version_matches("Mesa 26.3.1 (LLVM 21.1.2)"));
    const char *invalid[] = {NULL, "", "Mesa 26.3.10", "Mesa 26.3.1-rc1",
                             "Mesa 26.3.1-devel", "Mesa 26.3.1x", "NotMesa 26.3.1"};
    for (unsigned i = 0; i < sizeof(invalid) / sizeof(*invalid); ++i)
        assert(!mesa_version_matches(invalid[i]));

    int fds[2];
    assert(pipe(fds) == 0);
    assert(!dri2_redox_create_window_surface(NULL, NULL, (void *)(intptr_t)fds[0], NULL));
    assert(last_error == EGL_BAD_NATIVE_WINDOW);
    assert(fcntl(fds[0], F_GETFD) >= 0);
    assert(fcntl(borrowed_fd, F_GETFD) == -1 && errno == EBADF);
    close(fds[0]);
    close(fds[1]);

    struct dri2_egl_surface surface = {.orb_window = destination};
    char pixels[48];
    memset(pixels, 0x42, sizeof(pixels));
    for (int axis = 0; axis < 2; ++axis) {
        memset(destination, 0x17, sizeof(destination));
        last_error = 0;
        redox_swrast_put_image(NULL, __DRI_SWRAST_IMAGE_OP_SWAP,
                              axis ? 0 : -1, axis ? -1 : 0, 3, 4, pixels, &surface);
        assert(last_error == EGL_BAD_PARAMETER);
        for (unsigned i = 0; i < sizeof(destination); ++i)
            assert(destination[i] == 0x17);
    }
    last_error = 0;
    redox_swrast_put_image(NULL, __DRI_SWRAST_IMAGE_OP_SWAP, 2, 1, 3, 4, pixels, &surface);
    assert(!last_error);
    for (int y = 0; y < 4; ++y)
        for (int x = 0; x < 16; ++x)
            assert(destination[16 * y + x] == (y >= 1 && x >= 8 ? 0x42 : 0x17));
    return 0;
}
