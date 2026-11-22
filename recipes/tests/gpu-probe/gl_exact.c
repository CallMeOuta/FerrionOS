#define GL_GLES_PROTOTYPES 0
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <ctype.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "version.h"

#ifndef EGL_PLATFORM_SURFACELESS_MESA
#define EGL_PLATFORM_SURFACELESS_MESA 0x31DD
#endif
#ifndef EGL_NO_CONFIG_KHR
#define EGL_NO_CONFIG_KHR ((EGLConfig)0)
#endif

#define WIDTH 256
#define HEIGHT 256

int compare_pixels(const char *api, const unsigned char *pixels);

static const int corners[3][2] = {{258, 258}, {1794, 390}, {774, 1764}};
static const unsigned char background[4] = {15, 14, 12, 255};
static const unsigned char foreground[4] = {255, 153, 51, 255};

struct gles2_api {
    const GLubyte *(GL_APIENTRY *GetString)(GLenum name);
    GLenum (GL_APIENTRY *GetError)(void);
    void (GL_APIENTRY *GenTextures)(GLsizei n, GLuint *textures);
    void (GL_APIENTRY *BindTexture)(GLenum target, GLuint texture);
    void (GL_APIENTRY *TexImage2D)(GLenum target, GLint level, GLint internalformat,
                                   GLsizei width, GLsizei height, GLint border,
                                   GLenum format, GLenum type, const void *pixels);
    void (GL_APIENTRY *TexParameteri)(GLenum target, GLenum pname, GLint param);
    void (GL_APIENTRY *GenFramebuffers)(GLsizei n, GLuint *framebuffers);
    void (GL_APIENTRY *BindFramebuffer)(GLenum target, GLuint framebuffer);
    void (GL_APIENTRY *FramebufferTexture2D)(GLenum target, GLenum attachment,
                                             GLenum textarget, GLuint texture, GLint level);
    GLenum (GL_APIENTRY *CheckFramebufferStatus)(GLenum target);
    GLuint (GL_APIENTRY *CreateShader)(GLenum type);
    void (GL_APIENTRY *ShaderSource)(GLuint shader, GLsizei count,
                                     const GLchar *const *string, const GLint *length);
    void (GL_APIENTRY *CompileShader)(GLuint shader);
    void (GL_APIENTRY *GetShaderiv)(GLuint shader, GLenum pname, GLint *params);
    void (GL_APIENTRY *GetShaderInfoLog)(GLuint shader, GLsizei bufSize,
                                         GLsizei *length, GLchar *infoLog);
    void (GL_APIENTRY *DeleteShader)(GLuint shader);
    GLuint (GL_APIENTRY *CreateProgram)(void);
    void (GL_APIENTRY *AttachShader)(GLuint program, GLuint shader);
    void (GL_APIENTRY *BindAttribLocation)(GLuint program, GLuint index, const GLchar *name);
    void (GL_APIENTRY *LinkProgram)(GLuint program);
    void (GL_APIENTRY *GetProgramiv)(GLuint program, GLenum pname, GLint *params);
    void (GL_APIENTRY *GetProgramInfoLog)(GLuint program, GLsizei bufSize,
                                          GLsizei *length, GLchar *infoLog);
    void (GL_APIENTRY *UseProgram)(GLuint program);
    GLint (GL_APIENTRY *GetUniformLocation)(GLuint program, const GLchar *name);
    void (GL_APIENTRY *Uniform4f)(GLint location, GLfloat v0, GLfloat v1, GLfloat v2, GLfloat v3);
    void (GL_APIENTRY *Viewport)(GLint x, GLint y, GLsizei width, GLsizei height);
    void (GL_APIENTRY *Disable)(GLenum cap);
    void (GL_APIENTRY *ClearColor)(GLfloat red, GLfloat green, GLfloat blue, GLfloat alpha);
    void (GL_APIENTRY *Clear)(GLbitfield mask);
    void (GL_APIENTRY *VertexAttribPointer)(GLuint index, GLint size, GLenum type,
                                            GLboolean normalized, GLsizei stride, const void *pointer);
    void (GL_APIENTRY *EnableVertexAttribArray)(GLuint index);
    void (GL_APIENTRY *DrawArrays)(GLenum mode, GLint first, GLsizei count);
    void (GL_APIENTRY *Finish)(void);
    void (GL_APIENTRY *ReadPixels)(GLint x, GLint y, GLsizei width, GLsizei height,
                                   GLenum format, GLenum type, void *pixels);
};

static int load_gles2(struct gles2_api *gl)
{
#define LOAD(field, sym) do { \
    __eglMustCastToProperFunctionPointerType proc = eglGetProcAddress(#sym); \
    if (!proc) { \
        fprintf(stderr, "GPU_ERROR missing GLES2 symbol %s\n", #sym); \
        return 0; \
    } \
    memcpy(&gl->field, &proc, sizeof(proc)); \
} while (0)
    LOAD(GetString, glGetString);
    LOAD(GetError, glGetError);
    LOAD(GenTextures, glGenTextures);
    LOAD(BindTexture, glBindTexture);
    LOAD(TexImage2D, glTexImage2D);
    LOAD(TexParameteri, glTexParameteri);
    LOAD(GenFramebuffers, glGenFramebuffers);
    LOAD(BindFramebuffer, glBindFramebuffer);
    LOAD(FramebufferTexture2D, glFramebufferTexture2D);
    LOAD(CheckFramebufferStatus, glCheckFramebufferStatus);
    LOAD(CreateShader, glCreateShader);
    LOAD(ShaderSource, glShaderSource);
    LOAD(CompileShader, glCompileShader);
    LOAD(GetShaderiv, glGetShaderiv);
    LOAD(GetShaderInfoLog, glGetShaderInfoLog);
    LOAD(DeleteShader, glDeleteShader);
    LOAD(CreateProgram, glCreateProgram);
    LOAD(AttachShader, glAttachShader);
    LOAD(BindAttribLocation, glBindAttribLocation);
    LOAD(LinkProgram, glLinkProgram);
    LOAD(GetProgramiv, glGetProgramiv);
    LOAD(GetProgramInfoLog, glGetProgramInfoLog);
    LOAD(UseProgram, glUseProgram);
    LOAD(GetUniformLocation, glGetUniformLocation);
    LOAD(Uniform4f, glUniform4f);
    LOAD(Viewport, glViewport);
    LOAD(Disable, glDisable);
    LOAD(ClearColor, glClearColor);
    LOAD(Clear, glClear);
    LOAD(VertexAttribPointer, glVertexAttribPointer);
    LOAD(EnableVertexAttribArray, glEnableVertexAttribArray);
    LOAD(DrawArrays, glDrawArrays);
    LOAD(Finish, glFinish);
    LOAD(ReadPixels, glReadPixels);
#undef LOAD
    return 1;
}

static int contains_ci(const char *haystack, const char *needle)
{
    if (!haystack || !needle || !*needle)
        return 0;
    for (const char *h = haystack; *h; ++h) {
        const char *a = h;
        const char *b = needle;
        while (*a && *b && tolower((unsigned char)*a) == tolower((unsigned char)*b)) {
            ++a;
            ++b;
        }
        if (!*b)
            return 1;
    }
    return 0;
}

static GLuint compile_shader(const struct gles2_api *gl, GLenum type, const char *source)
{
    GLuint id = gl->CreateShader(type);
    GLint compiled = 0;
    gl->ShaderSource(id, 1, &source, NULL);
    gl->CompileShader(id);
    gl->GetShaderiv(id, GL_COMPILE_STATUS, &compiled);
    if (!compiled) {
        char log[2048];
        gl->GetShaderInfoLog(id, sizeof(log), NULL, log);
        fprintf(stderr, "GPU_ERROR GLES2 shader: %s\n", log);
        gl->DeleteShader(id);
        return 0;
    }
    return id;
}

int gl_exact(void)
{
    int result = 1;
    EGLDisplay display = eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
    if (display == EGL_NO_DISPLAY || !eglInitialize(display, NULL, NULL)) {
        fprintf(stderr, "GPU_ERROR EGL surfaceless initialize: %#x\n", eglGetError());
        return 1;
    }
    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        fprintf(stderr, "GPU_ERROR EGL bind GLES API: %#x\n", eglGetError());
        eglTerminate(display);
        return 1;
    }
    static const EGLint context_attrs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext context = eglCreateContext(display, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, context_attrs);
    if (context == EGL_NO_CONTEXT || !eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, context)) {
        fprintf(stderr, "GPU_ERROR EGL GLES2 context: %#x\n", eglGetError());
        if (context != EGL_NO_CONTEXT)
            eglDestroyContext(display, context);
        eglTerminate(display);
        return 1;
    }

    struct gles2_api gl;
    if (!load_gles2(&gl))
        goto cleanup;

    const char *renderer = (const char *)gl.GetString(GL_RENDERER);
    const char *version = (const char *)gl.GetString(GL_VERSION);
    if (!version) {
        fputs("GPU_ERROR missing GLES2 version\n", stderr);
        goto cleanup;
    }
    printf("GPU_GL_EXACT version=\"%s\" renderer=\"%s\"\n", version, renderer ? renderer : "");
    if (!contains_ci(renderer, "zink")) {
        fprintf(stderr, "GPU_ERROR expected zink renderer, got \"%s\"\n", renderer ? renderer : "");
        goto cleanup;
    }
    int version_ok = mesa_version_matches(version);
    if (!version_ok)
        fputs("GPU_VERSION expected Mesa 26.3.1\n", stderr);

    GLuint texture = 0, framebuffer = 0;
    gl.GenTextures(1, &texture);
    gl.BindTexture(GL_TEXTURE_2D, texture);
    gl.TexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, WIDTH, HEIGHT, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    gl.TexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    gl.GenFramebuffers(1, &framebuffer);
    gl.BindFramebuffer(GL_FRAMEBUFFER, framebuffer);
    gl.FramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
    if (gl.CheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        fputs("GPU_ERROR incomplete GLES2 framebuffer\n", stderr);
        goto cleanup;
    }

    GLuint vertex = compile_shader(&gl, GL_VERTEX_SHADER,
        "attribute vec2 p; void main(){ gl_Position = vec4(p, 0.0, 1.0); }");
    GLuint fragment = compile_shader(&gl, GL_FRAGMENT_SHADER,
        "precision mediump float; uniform vec4 c; void main(){ gl_FragColor = c; }");
    if (!vertex || !fragment)
        goto cleanup;

    GLuint program = gl.CreateProgram();
    gl.AttachShader(program, vertex);
    gl.AttachShader(program, fragment);
    gl.BindAttribLocation(program, 0, "p");
    gl.LinkProgram(program);
    GLint linked = 0;
    gl.GetProgramiv(program, GL_LINK_STATUS, &linked);
    if (!linked) {
        char log[2048];
        gl.GetProgramInfoLog(program, sizeof(log), NULL, log);
        fprintf(stderr, "GPU_ERROR GLES2 shader link failed: %s\n", log);
        goto cleanup;
    }
    gl.UseProgram(program);

    GLfloat positions[6];
    for (int i = 0; i < 3; ++i) {
        positions[2 * i] = corners[i][0] / 8.0f / (WIDTH / 2.0f) - 1.0f;
        positions[2 * i + 1] = corners[i][1] / 8.0f / (HEIGHT / 2.0f) - 1.0f;
    }
    gl.Viewport(0, 0, WIDTH, HEIGHT);
    gl.Disable(GL_DITHER);
    gl.ClearColor(background[0] / 255.0f, background[1] / 255.0f, background[2] / 255.0f, 1.0f);
    gl.Clear(GL_COLOR_BUFFER_BIT);
    gl.Uniform4f(gl.GetUniformLocation(program, "c"),
                 foreground[0] / 255.0f, foreground[1] / 255.0f, foreground[2] / 255.0f, 1.0f);
    gl.VertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, positions);
    gl.EnableVertexAttribArray(0);
    gl.DrawArrays(GL_TRIANGLES, 0, 3);
    gl.Finish();

    static unsigned char pixels[WIDTH * HEIGHT * 4];
    gl.ReadPixels(0, 0, WIDTH, HEIGHT, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    if (gl.GetError() != GL_NO_ERROR) {
        fputs("GPU_ERROR GLES2 draw or readback failed\n", stderr);
        goto cleanup;
    }

    result = compare_pixels("gl-exact", pixels) || !version_ok;
    if (!result)
        puts("GPU_GL_EXACT_OK");

cleanup:
    eglMakeCurrent(display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    eglDestroyContext(display, context);
    eglTerminate(display);
    return result;
}
