#include <pthread.h>
#include <stdio.h>
#include <unistd.h>
static pthread_mutex_t m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t c = PTHREAD_COND_INITIALIZER;
static int ready;
static void *worker(void *arg) {
    pthread_mutex_lock(&m); ready++; pthread_cond_signal(&c); pthread_mutex_unlock(&m);
    usleep(20000);
    return arg;
}
int main(void) {
    pthread_t t[4];
    for (int i = 0; i < 4; i++) pthread_create(&t[i], 0, worker, 0);
    pthread_mutex_lock(&m);
    while (ready < 4) pthread_cond_wait(&c, &m);
    pthread_mutex_unlock(&m);
    for (int i = 0; i < 4; i++) pthread_join(t[i], 0);
    puts("threads done");
    return 0;
}
