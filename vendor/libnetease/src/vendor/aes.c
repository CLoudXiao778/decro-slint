/* AES-128 encryption (FIPS-197). To eliminate transcription risk the S-box,
 * inverse tables and Rcon are all DERIVED at init from GF(2^8) arithmetic.
 * Only the test-suite vectors pin the behaviour down externally. */
#include "netease/aes.h"
#include "netease/util.h"
#include <string.h>

static uint8_t SBOX[256];
static uint8_t RCON[10];
static int aes_inited;

static uint8_t xtime(uint8_t a) {
    return (uint8_t)((a << 1) ^ ((a & 0x80) ? 0x1b : 0x00));
}
static uint8_t gf_mul(uint8_t a, uint8_t b) { /* bitwise multiply in GF(2^8) */
    uint8_t p = 0;
    while (b) {
        if (b & 1) p ^= a;
        a = xtime(a);
        b >>= 1;
    }
    return p;
}

static void aes_init_tables(void) {
    if (aes_inited) return;

    /* multiplicative inverses via brute force (256*256 worst case, once) */
    uint8_t inv[256];
    inv[0] = 0;
    for (int x = 1; x < 256; x++) {
        for (int y = 1; y < 256; y++) {
            if (gf_mul((uint8_t)x, (uint8_t)y) == 1) { inv[x] = (uint8_t)y; break; }
        }
    }
    /* affine transform: s = x ^ rotl(x,1) ^ rotl(x,2) ^ rotl(x,3) ^ rotl(x,4) ^ 0x63 */
    for (int x = 0; x < 256; x++) {
        uint8_t v = inv[x], s = v;
        s = (uint8_t)(s ^ ((v << 1) | (v >> 7)));
        s = (uint8_t)(s ^ ((v << 2) | (v >> 6)));
        s = (uint8_t)(s ^ ((v << 3) | (v >> 5)));
        s = (uint8_t)(s ^ ((v << 4) | (v >> 4)));
        SBOX[x] = (uint8_t)(s ^ 0x63);
    }
    /* Rcon[i] = x^(i-1) in GF(2^8) */
    uint8_t r = 0x01;
    for (int i = 0; i < 10; i++) { RCON[i] = r; r = xtime(r); }

    aes_inited = 1;
}

void ne_aes128_init(ne_aes128 *ctx, const uint8_t key[16]) {
    aes_init_tables();
    /* key expansion: 44 big-endian words as flat bytes */
    uint8_t *w = ctx->round_keys;
    memcpy(w, key, 16);
    for (int i = 4; i < 44; i++) {
        uint8_t t[4];
        memcpy(t, w + (i - 1) * 4, 4);
        if (i % 4 == 0) {
            /* RotWord */
            uint8_t tmp = t[0];
            t[0] = t[1]; t[1] = t[2]; t[2] = t[3]; t[3] = tmp;
            /* SubWord */
            for (int k = 0; k < 4; k++) t[k] = SBOX[t[k]];
            t[0] ^= RCON[i / 4 - 1];
        }
        for (int k = 0; k < 4; k++)
            w[i * 4 + k] = (uint8_t)(w[(i - 4) * 4 + k] ^ t[k]);
    }
}

void ne_aes128_encrypt_block(const ne_aes128 *ctx, const uint8_t in[16], uint8_t out[16]) {
    uint8_t s[16]; /* s[j], j = row + 4*col */
    const uint8_t *w = ctx->round_keys;

    for (int j = 0; j < 16; j++) s[j] = (uint8_t)(in[j] ^ w[j]);

    for (int round = 1; round <= 10; round++) {
        const uint8_t *rk = w + round * 16;

        /* SubBytes */
        for (int j = 0; j < 16; j++) s[j] = SBOX[s[j]];

        /* ShiftRows: row r rotates left by r */
        if (round < 10) { /* (also valid for round 10, but fold into same loop) */
        }
        uint8_t t[16];
        for (int r = 1; r < 4; r++)
            for (int c = 0; c < 4; c++)
                t[r + 4 * c] = s[r + 4 * ((c + r) % 4)];
        for (int r = 1; r < 4; r++)
            for (int c = 0; c < 4; c++)
                s[r + 4 * c] = t[r + 4 * c];

        /* MixColumns (skipped in the final round) */
        if (round < 10) {
            for (int c = 0; c < 4; c++) {
                uint8_t *p = s + 4 * c;
                uint8_t a0 = p[0], a1 = p[1], a2 = p[2], a3 = p[3];
                p[0] = (uint8_t)(gf_mul(a0, 2) ^ gf_mul(a1, 3) ^ a2 ^ a3);
                p[1] = (uint8_t)(a0 ^ gf_mul(a1, 2) ^ gf_mul(a2, 3) ^ a3);
                p[2] = (uint8_t)(a0 ^ a1 ^ gf_mul(a2, 2) ^ gf_mul(a3, 3));
                p[3] = (uint8_t)(gf_mul(a0, 3) ^ a1 ^ a2 ^ gf_mul(a3, 2));
            }
        }

        /* AddRoundKey */
        for (int j = 0; j < 16; j++) s[j] ^= rk[j];
    }
    memcpy(out, s, 16);
}

static uint8_t *aes_pad_encrypt(const uint8_t *in, size_t n, size_t *out_len,
                                int cbc, const uint8_t key[16], const uint8_t iv[16]) {
    size_t pad = 16 - (n % 16);            /* PKCS#7: always 1..16 bytes */
    size_t total = n + pad;
    uint8_t *buf = ne_xmalloc(total);
    memcpy(buf, in, n);
    memset(buf + n, (int)pad, pad);

    ne_aes128 ctx;
    ne_aes128_init(&ctx, key);

    uint8_t prev[16];
    if (cbc) memcpy(prev, iv, 16);

    for (size_t off = 0; off < total; off += 16) {
        if (cbc)
            for (int k = 0; k < 16; k++) buf[off + k] ^= prev[k];
        ne_aes128_encrypt_block(&ctx, buf + off, buf + off);
        if (cbc) memcpy(prev, buf + off, 16);
    }
    *out_len = total;
    return buf;
}

uint8_t *ne_aes_cbc_encrypt(const uint8_t *in, size_t n,
                            const uint8_t key[16], const uint8_t iv[16],
                            size_t *out_len) {
    return aes_pad_encrypt(in, n, out_len, 1, key, iv);
}
uint8_t *ne_aes_ecb_encrypt(const uint8_t *in, size_t n,
                            const uint8_t key[16], size_t *out_len) {
    return aes_pad_encrypt(in, n, out_len, 0, key, NULL);
}
