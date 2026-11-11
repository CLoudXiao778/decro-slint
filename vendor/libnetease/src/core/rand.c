#include "netease/rand.h"
#include "netease/util.h"
#include <string.h>

#ifndef _WIN32
#include <stdio.h>
int ne_rand_bytes(uint8_t *buf, size_t n) {
    FILE *f = fopen("/dev/urandom", "rb");
    if (!f) return -1;
    size_t got = fread(buf, 1, n, f);
    fclose(f);
    return got == n ? 0 : -1;
}
#else
#define _CRT_RAND_S
#include <stdlib.h>
int ne_rand_bytes(uint8_t *buf, size_t n) {
    for (size_t i = 0; i < n; i++) {
        unsigned int v;
        if (rand_s(&v) != 0) return -1;
        buf[i] = (uint8_t)v;
    }
    return 0;
}
#endif

uint32_t ne_rand_below(uint32_t n) {
    if (n < 2) return 0;
    /* rejection sampling on 32-bit draws */
    uint32_t limit = UINT32_MAX - (UINT32_MAX % n);
    for (;;) {
        uint32_t v;
        if (ne_rand_bytes((uint8_t *)&v, sizeof v) != 0) return v % n;
        if (v < limit) return v % n;
    }
}

void ne_rand_secret16(char out[17]) {
    static const char chars[] =
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    for (int i = 0; i < 16; i++) out[i] = chars[ne_rand_below(62)];
    out[16] = '\0';
}
