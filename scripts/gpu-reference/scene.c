/* SPDX-License-Identifier: MIT
 * FerrionOS GP0 exact scene; row zero is the bottom row returned by OpenGL.
 * Keep the published seed (1469598103934665603), which is not the standard
 * FNV-1a offset basis: changing it changes the reference hash.
 */
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#define W 256
#define H 256
static const int V[3][2] = {{258, 258}, {1794, 390}, {774, 1764}};
static const unsigned char BG[4] = {15, 14, 12, 255}, FG[4] = {255, 153, 51, 255};
static long long edge(int a, int b, long long x, long long y) {
    return (x - V[a][0]) * (long long)(V[b][1] - V[a][1]) - (y - V[a][1]) * (long long)(V[b][0] - V[a][0]);
}
int main(void) {
    EGLDisplay dpy = eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA, EGL_DEFAULT_DISPLAY, NULL);
    if (dpy == EGL_NO_DISPLAY || !eglInitialize(dpy, NULL, NULL)) return 1;
    eglBindAPI(EGL_OPENGL_ES_API);
    static const EGLint cattr[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    EGLContext ctx = eglCreateContext(dpy, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, cattr);
    if (ctx == EGL_NO_CONTEXT || !eglMakeCurrent(dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) return 1;
    GLuint tex, fbo; glGenTextures(1, &tex); glBindTexture(GL_TEXTURE_2D, tex);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, W, H, 0, GL_RGBA, GL_UNSIGNED_BYTE, NULL);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    glGenFramebuffers(1, &fbo); glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) return 1;
    const char *vs = "attribute vec2 p; void main(){ gl_Position = vec4(p, 0.0, 1.0); }";
    const char *fs = "precision mediump float; uniform vec4 c; void main(){ gl_FragColor = c; }";
    GLuint v = glCreateShader(GL_VERTEX_SHADER), f = glCreateShader(GL_FRAGMENT_SHADER), prog = glCreateProgram();
    glShaderSource(v, 1, &vs, NULL); glCompileShader(v); glShaderSource(f, 1, &fs, NULL); glCompileShader(f);
    glAttachShader(prog, v); glAttachShader(prog, f); glBindAttribLocation(prog, 0, "p"); glLinkProgram(prog);
    glUseProgram(prog);
    float pos[6];
    for (int i = 0; i < 3; i++) { pos[2*i] = V[i][0] / 8.0f / (W / 2.0f) - 1.0f; pos[2*i+1] = V[i][1] / 8.0f / (H / 2.0f) - 1.0f; }
    glViewport(0, 0, W, H); glDisable(GL_DITHER);
    glClearColor(BG[0] / 255.0f, BG[1] / 255.0f, BG[2] / 255.0f, 1.0f); glClear(GL_COLOR_BUFFER_BIT);
    glUniform4f(glGetUniformLocation(prog, "c"), FG[0] / 255.0f, FG[1] / 255.0f, FG[2] / 255.0f, 1.0f);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, pos); glEnableVertexAttribArray(0);
    glDrawArrays(GL_TRIANGLES, 0, 3); glFinish();
    static unsigned char px[W * H * 4]; glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    uint64_t hash = 1469598103934665603ULL;
    for (size_t i = 0; i < sizeof px; i++) { hash ^= px[i]; hash *= 1099511628211ULL; }
    long tri = 0, mism = 0, on_edge = 0;
    for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
        long long cx = 8 * x + 4, cy = 8 * y + 4, e0 = edge(0, 1, cx, cy), e1 = edge(1, 2, cx, cy), e2 = edge(2, 0, cx, cy);
        if (!e0 || !e1 || !e2) on_edge++;
        int inside = (e0 > 0 && e1 > 0 && e2 > 0) || (e0 < 0 && e1 < 0 && e2 < 0);
        const unsigned char *p = px + 4 * (y * W + x);
        tri += !memcmp(p, FG, 4);
        if (memcmp(p, inside ? FG : BG, 4)) mism++;
    }
    printf("GLTRI renderer=\"%s\" hash=%016llx tri=%ld mismatches=%ld centers_on_edge=%ld\n",
           (const char *)glGetString(GL_RENDERER), (unsigned long long)hash, tri, mism, on_edge);
    return mism ? 2 : 0;
}
