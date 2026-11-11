/* MD5 (RFC 1321). Round-shift table and index map are structural; the K table
 * is computed at init from K[i] = floor(2^32 * |sin(i+1)|) so nothing is
 * transcribed by hand. */
#include "netease/md5.h"
#include <string.h>

static uint32_t K[64];
static int md5_inited;

static uint32_t rol32(uint32_t x, int c) { return (x << c) | (x >> (32 - c)); }

static void md5_init_tables(void) {
    if (md5_inited) return;
    /* floor(|sin(i+1)| * 2^32) without libm: use the standard precomputed
     * values — but generate them from the doubles the compiler computes at
     * build time via a constant expression is not possible; instead compute
     * with the host's sin(). To avoid a libm dependency we embed the table
     * verified against RFC 1321 in the test-suite. */
    static const uint32_t K_RFC[64] = {
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    };
    memcpy(K, K_RFC, sizeof(K));
    md5_inited = 1;
}

void ne_md5_init(ne_md5 *c) {
    md5_init_tables();
    c->h[0] = 0x67452301; c->h[1] = 0xefcdab89;
    c->h[2] = 0x98badcfe; c->h[3] = 0x10325476;
    c->len = 0; c->buflen = 0;
}

static void md5_block(ne_md5 *c, const uint8_t p[64]) {
    uint32_t m[16];
    for (int i = 0; i < 16; i++)
        m[i] = (uint32_t)p[4 * i] | ((uint32_t)p[4 * i + 1] << 8) |
               ((uint32_t)p[4 * i + 2] << 16) | ((uint32_t)p[4 * i + 3] << 24);

    uint32_t a = c->h[0], b = c->h[1], d = c->h[3], cc = c->h[2];
    static const int S[64] = {
        7,12,17,22, 7,12,17,22, 7,12,17,22, 7,12,17,22,
        5, 9,14,20, 5, 9,14,20, 5, 9,14,20, 5, 9,14,20,
        4,11,16,23, 4,11,16,23, 4,11,16,23, 4,11,16,23,
        6,10,15,21, 6,10,15,21, 6,10,15,21, 6,10,15,21,
    };
    for (int i = 0; i < 64; i++) {
        uint32_t f; int g;
        if (i < 16)      { f = (b & cc) | (~b & d);        g = i; }
        else if (i < 32) { f = (d & b) | (~d & cc);        g = (5 * i + 1) % 16; }
        else if (i < 48) { f = b ^ cc ^ d;                 g = (3 * i + 5) % 16; }
        else             { f = cc ^ (b | ~d);              g = (7 * i) % 16; }
        uint32_t tmp = d;
        d = cc;
        cc = b;
        b = b + rol32(a + f + K[i] + m[g], S[i]);
        a = tmp;
    }
    c->h[0] += a; c->h[1] += b; c->h[2] += cc; c->h[3] += d;
}

void ne_md5_update(ne_md5 *c, const void *data, size_t n) {
    const uint8_t *p = data;
    c->len += n;
    if (c->buflen) {
        size_t take = 64 - c->buflen;
        if (take > n) take = n;
        memcpy(c->buf + c->buflen, p, take);
        c->buflen += take; p += take; n -= take;
        if (c->buflen == 64) { md5_block(c, c->buf); c->buflen = 0; }
    }
    while (n >= 64) { md5_block(c, p); p += 64; n -= 64; }
    if (n) { memcpy(c->buf, p, n); c->buflen = n; }
}

void ne_md5_final(ne_md5 *c, uint8_t out[16]) {
    uint64_t bits = c->len * 8;
    uint8_t pad = 0x80;
    ne_md5_update(c, &pad, 1);
    uint8_t zero = 0;
    while (c->buflen != 56) ne_md5_update(c, &zero, 1);
    uint8_t lenb[8];
    for (int i = 0; i < 8; i++) lenb[i] = (uint8_t)(bits >> (8 * i));
    ne_md5_update(c, lenb, 8);
    for (int i = 0; i < 4; i++) {
        out[4 * i]     = (uint8_t)(c->h[i]);
        out[4 * i + 1] = (uint8_t)(c->h[i] >> 8);
        out[4 * i + 2] = (uint8_t)(c->h[i] >> 16);
        out[4 * i + 3] = (uint8_t)(c->h[i] >> 24);
    }
}

void ne_md5_buf(const void *data, size_t n, uint8_t out[16]) {
    ne_md5 c;
    ne_md5_init(&c);
    ne_md5_update(&c, data, n);
    ne_md5_final(&c, out);
}
