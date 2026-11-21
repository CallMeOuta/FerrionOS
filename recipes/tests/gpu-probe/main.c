#define GL_GLEXT_PROTOTYPES
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GL/gl.h>
#include <GL/glext.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

/* Platform value supplied by Mesa's Orbital port. */
#define EGL_PLATFORM_REDOX_EXT 0x31D9
#define WIDTH 256
#define HEIGHT 256

static const int corners[3][2] = {{258, 258}, {1794, 390}, {774, 1764}};
static const unsigned char background[4] = {15, 14, 12, 255};
static const unsigned char foreground[4] = {255, 153, 51, 255};

static int64_t edge(int a, int b, int64_t x, int64_t y)
{
    return (x - corners[a][0]) * (corners[b][1] - corners[a][1])
        - (y - corners[a][1]) * (corners[b][0] - corners[a][0]);
}

static int compare_pixels(const unsigned char *pixels)
{
    uint64_t hash = UINT64_C(1469598103934665603);
    unsigned triangle = 0, mismatches = 0;
    for (unsigned i = 0; i < WIDTH * HEIGHT * 4; ++i) {
        hash = (hash ^ pixels[i]) * UINT64_C(1099511628211);
    }
    for (int y = 0; y < HEIGHT; ++y) {
        for (int x = 0; x < WIDTH; ++x) {
            int64_t cx = 8 * x + 4, cy = 8 * y + 4;
            int64_t a = edge(0, 1, cx, cy);
            int64_t b = edge(1, 2, cx, cy);
            int64_t c = edge(2, 0, cx, cy);
            int inside = (a > 0 && b > 0 && c > 0) || (a < 0 && b < 0 && c < 0);
            const unsigned char *pixel = pixels + 4 * (y * WIDTH + x);
            triangle += memcmp(pixel, foreground, 4) == 0;
            mismatches += memcmp(pixel, inside ? foreground : background, 4) != 0;
        }
    }
    printf("GPU_PIXELS triangle=%u mismatches=%u hash=%016" PRIx64 "\n",
           triangle, mismatches, hash);
    return triangle == 17540 && mismatches == 0 && hash == UINT64_C(0x47972063c7a47bfb) ? 0 : 1;
}

static GLuint shader(GLenum type, const char *source)
{
    GLuint result = glCreateShader(type);
    GLint compiled = 0;
    glShaderSource(result, 1, &source, NULL);
    glCompileShader(result);
    glGetShaderiv(result, GL_COMPILE_STATUS, &compiled);
    if (!compiled) {
        char log[2048];
        glGetShaderInfoLog(result, sizeof(log), NULL, log);
        fprintf(stderr, "GPU_ERROR shader: %s\n", log);
        glDeleteShader(result);
        return 0;
    }
    return result;
}

static int egl_dispatch(void)
{
    EGLDisplay display = eglGetPlatformDisplay(EGL_PLATFORM_REDOX_EXT, EGL_DEFAULT_DISPLAY, NULL);
    if (display == EGL_NO_DISPLAY || !eglInitialize(display, NULL, NULL)) {
        fprintf(stderr, "GPU_ERROR EGL initialize: %#x\n", eglGetError());
        return 1;
    }
    const EGLint attributes[] = {EGL_RENDERABLE_TYPE, EGL_OPENGL_BIT, EGL_NONE};
    EGLConfig config;
    EGLint count = 0;
    if (!eglBindAPI(EGL_OPENGL_API) || !eglChooseConfig(display, attributes, &config, 1, &count) || count != 1) {
        fprintf(stderr, "GPU_ERROR EGL OpenGL config: %#x\n", eglGetError());
        return 1;
    }
    EGLContext context = eglCreateContext(display, config, EGL_NO_CONTEXT, NULL);
    if (context == EGL_NO_CONTEXT || !eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, context)) {
        fprintf(stderr, "GPU_ERROR EGL context: %#x\n", eglGetError());
        return 1;
    }
    const char *renderer = (const char *)glGetString(GL_RENDERER);
    printf("GPU_GL version=\"%s\" renderer=\"%s\"\n", glGetString(GL_VERSION), renderer);
    if (!renderer || !strstr(renderer, "llvmpipe")) {
        fputs("GPU_ERROR expected llvmpipe\n", stderr);
        return 1;
    }
    GLuint texture, framebuffer;
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, WIDTH, HEIGHT, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glGenFramebuffers(1, &framebuffer);
    glBindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        fputs("GPU_ERROR incomplete framebuffer\n", stderr);
        return 1;
    }
    GLuint vertex = shader(GL_VERTEX_SHADER,
        "#version 120\nattribute vec2 p; void main() { gl_Position = vec4(p, 0.0, 1.0); }");
    GLuint fragment = shader(GL_FRAGMENT_SHADER,
        "#version 120\nuniform vec4 color; void main() { gl_FragColor = color; }");
    if (!vertex || !fragment) {
        return 1;
    }
    GLuint program = glCreateProgram();
    glAttachShader(program, vertex);
    glAttachShader(program, fragment);
    glBindAttribLocation(program, 0, "p");
    glLinkProgram(program);
    GLint linked = 0;
    glGetProgramiv(program, GL_LINK_STATUS, &linked);
    if (!linked) {
        fputs("GPU_ERROR shader link failed\n", stderr);
        return 1;
    }
    glUseProgram(program);
    GLfloat positions[6];
    for (int i = 0; i < 3; ++i) {
        positions[2 * i] = corners[i][0] / 8.0f / (WIDTH / 2.0f) - 1.0f;
        positions[2 * i + 1] = corners[i][1] / 8.0f / (HEIGHT / 2.0f) - 1.0f;
    }
    glViewport(0, 0, WIDTH, HEIGHT);
    glDisable(GL_DITHER);
    glClearColor(background[0] / 255.0f, background[1] / 255.0f, background[2] / 255.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glUniform4f(glGetUniformLocation(program, "color"), foreground[0] / 255.0f,
                foreground[1] / 255.0f, foreground[2] / 255.0f, 1.0f);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, positions);
    glEnableVertexAttribArray(0);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    static unsigned char pixels[WIDTH * HEIGHT * 4];
    glReadPixels(0, 0, WIDTH, HEIGHT, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    if (glGetError() != GL_NO_ERROR) {
        fputs("GPU_ERROR OpenGL draw or readback failed\n", stderr);
        return 1;
    }
    int result = compare_pixels(pixels);
    eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(display, context);
    eglTerminate(display);
    return result;
}

int main(int argc, char **argv)
{
    if (argc == 2 && strcmp(argv[1], "egl-dispatch") == 0) {
        return egl_dispatch();
    }
    fputs("Usage: gpu-probe egl-dispatch\n", stderr);
    return 2;
}
