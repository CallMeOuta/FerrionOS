#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    puts("dlopen-tls: begin dlopen");
    void *library = dlopen("/usr/lib/libdlopen-tls-worker.so", RTLD_NOW | RTLD_LOCAL);
    if (!library) {
        printf("dlopen-tls: FAIL dlopen (%s)\n", dlerror());
        return 1;
    }
    void *occupant = dlopen("/usr/lib/libdlopen-tls-occupant.so", RTLD_NOW | RTLD_LOCAL);
    if (!occupant) {
        printf("dlopen-tls: FAIL occupant-dlopen (%s)\n", dlerror());
        return 1;
    }
    puts("dlopen-tls: pass dlopen");

    void *(*worker)(void *) = (void *(*)(void *))dlsym(library, "dlopen_tls_worker");
    if (!worker) {
        printf("dlopen-tls: FAIL dlsym (%s)\n", dlerror());
        return 1;
    }

    puts("dlopen-tls: begin pthread-after-dlopen");
    pthread_t thread;
    int error = pthread_create(&thread, NULL, worker, NULL);
    if (error) {
        printf("dlopen-tls: FAIL pthread-create (%d)\n", error);
        return 1;
    }
    void *result = NULL;
    error = pthread_join(thread, &result);
    if (error || result) {
        printf("dlopen-tls: FAIL pthread-join (error %d, result %p)\n", error, result);
        return 1;
    }

    if (dlclose(occupant) != 0 || dlclose(library) != 0) {
        printf("dlopen-tls: FAIL dlclose (%s)\n", dlerror());
        return 1;
    }
    puts("dlopen-tls: PASS");
    return 0;
}
