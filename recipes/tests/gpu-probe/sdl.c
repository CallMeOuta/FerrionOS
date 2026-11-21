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
            SDL_GL_SwapWindow(window);
        }
        SDL_GL_DeleteContext(context);
        SDL_DestroyWindow(window);
    }
    SDL_Quit();
    puts("GPU_SDL_LIFECYCLE_OK");
    return 0;
}
