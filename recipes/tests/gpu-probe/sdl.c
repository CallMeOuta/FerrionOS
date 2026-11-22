#include <SDL.h>
#include <SDL_opengl.h>
#include <stdio.h>

int sdl_lifecycle(void)
{
    if (SDL_Init(SDL_INIT_VIDEO)) {
        fprintf(stderr, "GPU_ERROR SDL initialize: %s\n", SDL_GetError());
        return 1;
    }
    for (int pass = 0; pass < 2; pass++) {
        SDL_Window *window = SDL_CreateWindow("EGL lifecycle", 32, 32, 128, 128,
                                             SDL_WINDOW_OPENGL | SDL_WINDOW_RESIZABLE);
        SDL_GLContext context = window ? SDL_GL_CreateContext(window) : NULL;
        if (!context) {
            fprintf(stderr, "GPU_ERROR SDL context: %s\n", SDL_GetError());
            return 1;
        }
        for (int width = 128; width <= 192; width += 64) {
            unsigned char pixel[4] = {0};
            SDL_SetWindowSize(window, width, width);
            SDL_PumpEvents();
            glViewport(0, 0, width, width);
            glClearColor(1.0f, 0.0f, 0.0f, 1.0f);
            glClear(GL_COLOR_BUFFER_BIT);
            glReadPixels(width - 1, width - 1, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
            if (glGetError() != GL_NO_ERROR || pixel[0] != 255 || pixel[1] || pixel[2]) {
                fprintf(stderr, "GPU_ERROR SDL resized frame %d: %u,%u,%u,%u\n",
                        width, pixel[0], pixel[1], pixel[2], pixel[3]);
                return 1;
            }
            SDL_ClearError();
            SDL_GL_SwapWindow(window);
            if (*SDL_GetError()) {
                fprintf(stderr, "GPU_ERROR SDL swap: %s\n", SDL_GetError());
                return 1;
            }
        }
        SDL_GL_DeleteContext(context);
        SDL_DestroyWindow(window);
    }
    SDL_Quit();
    puts("GPU_SDL_LIFECYCLE_OK");
    return 0;
}

int sdl_recreate(void)
{
    if (SDL_Init(SDL_INIT_VIDEO)) {
        fprintf(stderr, "GPU_ERROR SDL initialize: %s\n", SDL_GetError());
        return 1;
    }
    if (SDL_GL_LoadLibrary(NULL) != 0) {
        fprintf(stderr, "GPU_ERROR SDL load library: %s\n", SDL_GetError());
        SDL_Quit();
        return 1;
    }
    SDL_Window *first = SDL_CreateWindow("EGL recreate 1", 32, 32, 128, 128,
                                         SDL_WINDOW_OPENGL | SDL_WINDOW_RESIZABLE);
    SDL_GLContext context = first ? SDL_GL_CreateContext(first) : NULL;
    if (!context) {
        fprintf(stderr, "GPU_ERROR SDL context: %s\n", SDL_GetError());
        if (first)
            SDL_DestroyWindow(first);
        SDL_GL_UnloadLibrary();
        SDL_Quit();
        return 1;
    }
    SDL_DestroyWindow(first);

    SDL_Window *second = SDL_CreateWindow("EGL recreate 2", 32, 32, 128, 128,
                                          SDL_WINDOW_OPENGL | SDL_WINDOW_RESIZABLE);
    if (!second) {
        fprintf(stderr, "GPU_ERROR SDL second window: %s\n", SDL_GetError());
        SDL_GL_DeleteContext(context);
        SDL_GL_UnloadLibrary();
        SDL_Quit();
        return 1;
    }
    if (SDL_GL_MakeCurrent(second, context) != 0) {
        fprintf(stderr, "GPU_ERROR SDL make current: %s\n", SDL_GetError());
        SDL_GL_DeleteContext(context);
        SDL_DestroyWindow(second);
        SDL_GL_UnloadLibrary();
        SDL_Quit();
        return 1;
    }

    unsigned char pixel[4] = {0};
    glViewport(0, 0, 128, 128);
    glClearColor(0.0f, 1.0f, 0.0f, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();
    glReadPixels(64, 64, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, pixel);
    GLenum gl_err = glGetError();
    int pixel_ok = (gl_err == GL_NO_ERROR && pixel[0] == 0 && pixel[1] == 255 && pixel[2] == 0);
    if (!pixel_ok) {
        fprintf(stderr, "GPU_ERROR SDL recreate pixel err=%#x rgba=%u,%u,%u,%u\n",
                gl_err, pixel[0], pixel[1], pixel[2], pixel[3]);
    }

    SDL_ClearError();
    SDL_GL_SwapWindow(second);
    const char *swap_err = SDL_GetError();
    int swap_ok = (!swap_err || !*swap_err);
    if (!swap_ok) {
        fprintf(stderr, "GPU_ERROR SDL recreate swap: %s\n", swap_err);
    }

    SDL_GL_DeleteContext(context);
    SDL_DestroyWindow(second);
    SDL_GL_UnloadLibrary();
    SDL_Quit();
    if (!pixel_ok || !swap_ok) {
        return 1;
    }
    puts("GPU_SDL_RECREATE_OK");
    return 0;
}
