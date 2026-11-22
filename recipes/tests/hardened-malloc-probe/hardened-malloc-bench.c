#define _GNU_SOURCE
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/statvfs.h>
#include <time.h>

enum {
    SMALL_LIVE = 1024,
    SMALL_ITERS = 100000,
    MEDIUM_LIVE = 64,
    MEDIUM_ITERS = 8000,
    LARGE_ITERS = 128,
    REALLOC_ITERS = 400,
};

static uint64_t now_us(void) {
    struct timespec ts;
    int rc = clock_gettime(CLOCK_MONOTONIC, &ts);
    assert(rc == 0);
    return (uint64_t)ts.tv_sec * 1000000ULL + (uint64_t)ts.tv_nsec / 1000ULL;
}

static uint64_t used_frames(void) {
    struct statvfs st;
    if (statvfs("/scheme/memory", &st) != 0) {
        return 0;
    }
    return st.f_blocks >= st.f_bfree ? (uint64_t)(st.f_blocks - st.f_bfree) : 0;
}

static uint32_t xorshift32(uint32_t *state) {
    uint32_t x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    return x;
}

int main(int argc, char **argv) {
    const char *label = argc >= 2 ? argv[1] : "unknown";
    if (argc >= 3 && strcmp(argv[2], "--nop") == 0) {
        void *p = malloc(64);
        assert(p != NULL);
        memset(p, 1, 64);
        free(p);
        printf("bench-nop label=%s ok=1\n", label);
        return 0;
    }

    uint64_t f_start = used_frames();
    uint32_t rng = 0x12345678U;
    void *small_slots[SMALL_LIVE];
    memset(small_slots, 0, sizeof(small_slots));

    uint64_t t0 = now_us();
    uint64_t f_small_peak = f_start;
    for (int i = 0; i < SMALL_ITERS; i++) {
        size_t idx = xorshift32(&rng) % SMALL_LIVE;
        if (small_slots[idx] != NULL) {
            free(small_slots[idx]);
        }
        size_t sz = 16 + (xorshift32(&rng) % 2033);
        unsigned char *p = malloc(sz);
        assert(p != NULL);
        p[0] = (unsigned char)i;
        p[sz - 1] = (unsigned char)(i ^ 0x5A);
        small_slots[idx] = p;
        if ((i & 4095) == 0) {
            uint64_t cur = used_frames();
            if (cur > f_small_peak) {
                f_small_peak = cur;
            }
        }
    }
    for (int i = 0; i < SMALL_LIVE; i++) {
        free(small_slots[i]);
    }
    uint64_t t1 = now_us();
    uint64_t f_after_small = used_frames();

    void *med_slots[MEDIUM_LIVE];
    memset(med_slots, 0, sizeof(med_slots));
    uint64_t t2 = now_us();
    uint64_t f_med_peak = f_after_small;
    for (int i = 0; i < MEDIUM_ITERS; i++) {
        size_t idx = xorshift32(&rng) % MEDIUM_LIVE;
        if (med_slots[idx] != NULL) {
            free(med_slots[idx]);
        }
        size_t sz = 8192 + (xorshift32(&rng) % 57344);
        unsigned char *p = malloc(sz);
        assert(p != NULL);
        p[0] = (unsigned char)i;
        p[sz - 1] = (unsigned char)i;
        med_slots[idx] = p;
        if ((i & 511) == 0) {
            uint64_t cur = used_frames();
            if (cur > f_med_peak) {
                f_med_peak = cur;
            }
        }
    }
    for (int i = 0; i < MEDIUM_LIVE; i++) {
        free(med_slots[i]);
    }
    uint64_t t3 = now_us();
    uint64_t f_after_med = used_frames();

    uint64_t t4 = now_us();
    uint64_t f_large_peak = f_after_med;
    for (int i = 0; i < LARGE_ITERS; i++) {
        size_t sz = 262144 + ((size_t)(i & 3) * 131072);
        unsigned char *p = malloc(sz);
        assert(p != NULL);
        p[0] = (unsigned char)i;
        p[sz - 1] = (unsigned char)i;
        if ((i & 15) == 0) {
            uint64_t cur = used_frames();
            if (cur > f_large_peak) {
                f_large_peak = cur;
            }
        }
        free(p);
    }
    uint64_t t5 = now_us();

    uint64_t t6 = now_us();
    for (int i = 0; i < REALLOC_ITERS; i++) {
        size_t sz = 32;
        unsigned char *p = malloc(sz);
        assert(p != NULL);
        p[0] = 0x11;
        for (int step = 0; step < 12; step++) {
            sz *= 2;
            p = realloc(p, sz);
            assert(p != NULL);
            p[sz - 1] = (unsigned char)step;
        }
        free(p);
    }
    uint64_t t7 = now_us();
    uint64_t f_end = used_frames();

    printf("bench label=%s small_ms=%llu med_ms=%llu large_ms=%llu realloc_ms=%llu small_peak_kib=%lld med_peak_kib=%lld large_peak_kib=%lld retained_kib=%lld\n",
           label,
           (unsigned long long)((t1 - t0) / 1000ULL),
           (unsigned long long)((t3 - t2) / 1000ULL),
           (unsigned long long)((t5 - t4) / 1000ULL),
           (unsigned long long)((t7 - t6) / 1000ULL),
           (long long)(((int64_t)f_small_peak - (int64_t)f_start) * 4LL),
           (long long)(((int64_t)f_med_peak - (int64_t)f_start) * 4LL),
           (long long)(((int64_t)f_large_peak - (int64_t)f_start) * 4LL),
           (long long)(((int64_t)f_end - (int64_t)f_start) * 4LL));
    return 0;
}
