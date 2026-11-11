/* Faithful port of github.com/skip2/go-qrcode (master, 2020-06 commit da1b656)
 * covering exactly what netease-cli uses:
 *   qrcode.New(content, qrcode.Medium).ToSmallString(false) / .PNG(480)
 *
 * Ported 1:1 from encoder.go / qrcode.go / regular_symbol.go / symbol.go /
 * reedsolomon/{gf2_8,gf_poly,reed_solomon}.go — including the library's own
 * quirks (penalty3's 0xFF buffer reset, single-segment preference on length
 * ties, first-lowest mask wins) so the module bitmap is identical to Go's. */
#include "netease/qrenc.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "qr_medium_table.h"

#ifdef NE_HAVE_ZLIB
#include <zlib.h>
#endif

/* ════════════════ bitset (bitset/bitset.go subset) ════════════════ */
typedef struct {
    unsigned char *b; /* one bit per element, MSB-first within uint32? No:
                         Go bitset stores individual bools; we store bytes. */
    size_t len, cap;
} bits_t;

static void bts_init(bits_t *s) { s->b = NULL; s->len = s->cap = 0; }
static void bts_free(bits_t *s) { free(s->b); s->b = NULL; s->len = s->cap = 0; }
static void bts_reserve(bits_t *s, size_t n) {
    if (n <= s->cap) return;
    size_t nc = s->cap ? s->cap * 2 : 64;
    while (nc < n) nc *= 2;
    s->b = realloc(s->b, nc);
    s->cap = nc;
}
static void bts_append_bit(bits_t *s, int v) {
    bts_reserve(s, s->len + 1);
    s->b[s->len++] = v ? 1 : 0;
}
static void bts_append_bits(bits_t *s, const unsigned char *v, size_t n) {
    bts_reserve(s, s->len + n);
    for (size_t i = 0; i < n; i++) s->b[s->len + i] = v[i] ? 1 : 0;
    s->len += n;
}
/* AppendUint32(v, n) — n bits of v, most significant bit first */
static void bts_append_u32(bits_t *s, uint32_t v, int n) {
    for (int i = n - 1; i >= 0; i--) bts_append_bit(s, (int)((v >> i) & 1));
}
static int bts_at(const bits_t *s, size_t i) { return s->b[i]; }
/* ByteAt(j) — 8 bits at offset j (caller guarantees a full byte) */
static unsigned char bts_byte_at(const bits_t *s, size_t j) {
    unsigned char v = 0;
    for (int k = 0; k < 8; k++) v = (unsigned char)((v << 1) | (s->b[j + k] ? 1 : 0));
    return v;
}
static bits_t bts_substr(const bits_t *s, size_t a, size_t b) {
    bits_t out;
    bts_init(&out);
    bts_append_bits(&out, s->b + a, b - a);
    return out;
}
static bits_t bts_clone(const bits_t *s) { return bts_substr(s, 0, s->len); }
static void bts_append_false(bits_t *s, size_t n) {
    bts_reserve(s, s->len + n);
    memset(s->b + s->len, 0, n);
    s->len += n;
}

/* ════════════════ data encoder (encoder.go) ════════════════ */
enum { MODE_NONE = 1, MODE_NUM = 2, MODE_ALNUM = 4, MODE_BYTE = 8 };

typedef struct {
    int min_version, max_version;
    int num_numeric_cc_bits, num_alnum_cc_bits, num_byte_cc_bits;
} enc_type_t;

static const enc_type_t ENC_1_9 = {1, 9, 10, 9, 8};
static const enc_type_t ENC_10_26 = {10, 26, 12, 11, 16};
static const enc_type_t ENC_27_40 = {27, 40, 14, 13, 16};

typedef struct {
    int mode;
    const unsigned char *data;
    size_t len;
} segment_t;

static int alnum_value(unsigned char v) {
    if (v >= '0' && v <= '9') return v - '0';
    if (v >= 'A' && v <= 'Z') return v - 'A' + 10;
    switch (v) {
        case ' ': return 36;
        case '$': return 37;
        case '%': return 38;
        case '*': return 39;
        case '+': return 40;
        case '-': return 41;
        case '.': return 42;
        case '/': return 43;
        case ':': return 44;
    }
    return -1; /* unreachable: classifier guarantees membership */
}

static int mode_indicator(int mode) {
    switch (mode) { /* 4-bit indicator, MSB first */
        case MODE_NUM:   return 0x1;
        case MODE_ALNUM: return 0x2;
        default:         return 0x4;
    }
}

static int char_count_bits(const enc_type_t *t, int mode) {
    switch (mode) {
        case MODE_NUM:   return t->num_numeric_cc_bits;
        case MODE_ALNUM: return t->num_alnum_cc_bits;
        default:         return t->num_byte_cc_bits;
    }
}

static int classify_mode(unsigned char v) {
    if (v >= 0x30 && v <= 0x39) return MODE_NUM;
    if (v == 0x20 || v == 0x24 || v == 0x25 || v == 0x2a || v == 0x2b || v == 0x2d ||
        v == 0x2e || v == 0x2f || v == 0x3a || (v >= 0x41 && v <= 0x5a))
        return MODE_ALNUM;
    return MODE_BYTE;
}

/* encodedLength() — returns -1 for the "length too long" error */
static long encoded_length(const enc_type_t *t, int mode, size_t n) {
    int cc = char_count_bits(t, mode);
    if (n > (size_t)((1 << cc) - 1)) return -1;
    long length = 4 + cc;
    switch (mode) {
        case MODE_NUM:
            length += 10 * (long)(n / 3);
            if (n % 3 != 0) length += 1 + 3 * (n % 3);
            break;
        case MODE_ALNUM:
            length += 11 * (long)(n / 2);
            length += 6 * (long)(n % 2);
            break;
        default:
            length += 8 * (long)n;
    }
    return length;
}

/* encode() — full classify + optimise + single-segment preference.
 * Returns 0 on success, fills `encoded`; non-zero error otherwise. */
static int data_encode(const enc_type_t *t, const unsigned char *data, size_t n,
                       bits_t *encoded, const char **err) {
    if (n == 0) { *err = "no data to encode"; return -1; }

    /* classify into actual segments */
    segment_t actual[2048];
    size_t n_actual = 0;
    int mode = MODE_NONE, highest = MODE_NONE;
    size_t start = 0;
    for (size_t i = 0; i < n; i++) {
        int nm = classify_mode(data[i]);
        if (nm != mode) {
            if (i > 0) {
                actual[n_actual].mode = mode;
                actual[n_actual].data = data + start;
                actual[n_actual].len = i - start;
                n_actual++;
            }
            mode = nm;
            if (i > 0) start = i;
        }
        if (nm > highest) highest = nm;
    }
    actual[n_actual].mode = mode;
    actual[n_actual].data = data + start;
    actual[n_actual].len = n - start;
    n_actual++;

    /* optimiseDataModes(): coalesce compatible adjacent segments */
    segment_t optimised[2048];
    size_t n_opt = 0;
    size_t i = 0;
    while (i < n_actual) {
        int m = actual[i].mode;
        size_t num_chars = actual[i].len;
        size_t j = i + 1;
        while (j < n_actual) {
            if (actual[j].mode > m) break;
            long coalesced = encoded_length(t, m, num_chars + actual[j].len);
            long sep1 = encoded_length(t, m, num_chars);
            long sep2 = encoded_length(t, actual[j].mode, actual[j].len);
            if (coalesced < 0 || sep1 < 0 || sep2 < 0) {
                *err = "length too long to be represented";
                return -1;
            }
            if (coalesced < sep1 + sep2) {
                j++;
                num_chars += actual[j - 1].len;
            } else {
                break;
            }
        }
        optimised[n_opt].mode = m;
        optimised[n_opt].len = num_chars; /* lengths only; data merged below */
        optimised[n_opt].data = NULL;
        n_opt++;
        i = j;
    }

    /* single-segment check: "singleByteSegmentLength <= optimizedLength" keeps
     * the single segment on ties (Go behaviour) */
    long optimized_length = 0;
    for (size_t k = 0; k < n_opt; k++) {
        long l = encoded_length(t, optimised[k].mode, optimised[k].len);
        if (l < 0) { *err = "length too long to be represented"; return -1; }
        optimized_length += l;
    }
    long single = encoded_length(t, highest, n);
    if (single < 0) { *err = "length too long to be represented"; return -1; }
    if (single <= optimized_length) {
        optimised[0].mode = highest;
        optimised[0].len = n;
        n_opt = 1;
    }

    /* encode segments: ONE mode indicator + char count per *optimised*
     * segment; its data is the contiguous span actual[a..b) merged into it.
     * Track spans during the optimise pass itself (below). */
    size_t span_end[2048];
    {
        size_t si = 0;
        for (size_t k = 0; k < n_opt; k++) {
            size_t chars = actual[si].len;
            size_t j2 = si + 1;
            while (chars < optimised[k].len && j2 < n_actual) chars += actual[j2++].len;
            span_end[k] = j2;
            si = j2;
        }
    }
    bts_init(encoded);
    size_t span_start = 0;
    for (size_t k = 0; k < n_opt; k++) {
        size_t j2 = span_end[k];
        /* contiguous byte range of the merged span */
        const unsigned char *d = actual[span_start].data;
        size_t dn = 0;
        for (size_t z = span_start; z < j2; z++) dn += actual[z].len;

        bts_append_u32(encoded, (uint32_t)mode_indicator(optimised[k].mode), 4);
        bts_append_u32(encoded, (uint32_t)dn, char_count_bits(t, optimised[k].mode));

        switch (optimised[k].mode) {
            case MODE_NUM:
                for (size_t x = 0; x < dn; x += 3) {
                    size_t rem = dn - x;
                    uint32_t v = 0;
                    int bu = 1;
                    for (size_t y = 0; y < rem && y < 3; y++) {
                        v *= 10;
                        v += (uint32_t)(d[x + y] - 0x30);
                        bu += 3;
                    }
                    bts_append_u32(encoded, v, bu);
                }
                break;
            case MODE_ALNUM:
                for (size_t x = 0; x < dn; x += 2) {
                    size_t rem = dn - x;
                    uint32_t v = 0;
                    for (size_t y = 0; y < rem && y < 2; y++)
                        v = v * 45 + (uint32_t)alnum_value(d[x + y]);
                    bts_append_u32(encoded, v, rem > 1 ? 11 : 6);
                }
                break;
            default:
                for (size_t x = 0; x < dn; x++)
                    bts_append_u32(encoded, d[x], 8);
        }
        span_start = j2;
    }
    return 0;
}

/* ════════════════ GF(2^8) + Reed-Solomon (reedsolomon package) ════════════════ */
static unsigned char gf_exp[512];
static unsigned char gf_log[256];
static int gf_ready = 0;

static void gf_init(void) {
    if (gf_ready) return;
    unsigned x = 1;
    for (int i = 0; i < 255; i++) {
        gf_exp[i] = (unsigned char)x;
        gf_log[x] = (unsigned char)i;
        x <<= 1;
        if (x & 0x100) x ^= 0x11d; /* x^8 + x^4 + x^3 + x^2 + 1 */
    }
    for (int i = 255; i < 512; i++) gf_exp[i] = gf_exp[i - 255];
    gf_ready = 1;
}
static unsigned char gf_mul(unsigned a, unsigned b) {
    if (a == 0 || b == 0) return 0;
    return gf_exp[(unsigned)gf_log[a] + (unsigned)gf_log[b]];
}
static unsigned char gf_div(unsigned a, unsigned b) {
    if (a == 0) return 0;
    return gf_exp[(unsigned)gf_log[a] + 255 - (unsigned)gf_log[b]];
}

typedef struct { unsigned char *t; int n; } poly_t; /* t[i] = coeff of x^i */

static poly_t poly_new(int n) {
    poly_t p; p.t = calloc((size_t)n, 1); p.n = n; return p;
}
static void poly_free(poly_t *p) { free(p->t); p->t = NULL; p->n = 0; }
static void poly_trim(poly_t *p) { /* normalised(): drop high zero terms */
    int last = p->n - 1;
    while (last >= 0 && p->t[last] == 0) last--;
    p->n = last + 1; /* may become 0 == empty poly (matches gfPoly{}) */
}
static poly_t poly_mul(poly_t a, poly_t b) {
    poly_t r = poly_new(a.n + b.n);
    for (int i = 0; i < a.n; i++) {
        if (a.t[i] == 0) continue;
        for (int j = 0; j < b.n; j++) {
            if (b.t[j] == 0) continue;
            unsigned char m = gf_mul(a.t[i], b.t[j]);
            /* add monomial m*x^(i+j) */
            r.t[i + j] ^= m; /* GF(2^8) addition == xor; equals Go's
                                gfPolyAdd path because adding the same
                                monomial twice is impossible here (each
                                (i,j) pair is visited once) */
        }
    }
    poly_trim(&r);
    return r;
}
/* RS generator: (x + a^0)(x + a^1)...(x + a^(degree-1)) */
static poly_t rs_generator(int degree) {
    poly_t g = poly_new(1);
    g.t[0] = 1;
    for (int i = 0; i < degree; i++) {
        poly_t next = poly_new(2);
        next.t[0] = gf_exp[i];
        next.t[1] = 1;
        poly_t prod = poly_mul(g, next);
        poly_free(&g);
        poly_free(&next);
        g = prod;
    }
    return g;
}
static poly_t poly_remainder(poly_t num, poly_t den) {
    poly_t rem = poly_new(num.n);
    memcpy(rem.t, num.t, (size_t)num.n);
    while (rem.n >= den.n && den.n > 0) {
        int degree = rem.n - den.n;
        unsigned char coeff = gf_div(rem.t[rem.n - 1], den.t[den.n - 1]);
        /* divisor = den * coeff * x^degree; subtract == xor in GF(2^8) */
        for (int i = 0; i < den.n; i++)
            rem.t[i + degree] ^= gf_mul(den.t[i], coeff);
        poly_trim(&rem);
        if (rem.n == 0) break;
    }
    return rem;
}

/* reedsolomon.Encode(data, numECBytes): data + numECBytes remainder bytes */
static void rs_encode(const bits_t *data, int num_ec_bytes, bits_t *out) {
    gf_init();
    int n = (int)((data->len + 7) / 8);
    poly_t ecp = poly_new(n);
    for (int i = n - 1, j = 0; j < (int)data->len; i--, j += 8)
        ecp.t[i] = bts_byte_at(data, (size_t)j);

    /* ecpoly * x^numECBytes  (== shift coefficients up) */
    poly_t shifted = poly_new(n + num_ec_bytes);
    for (int i = 0; i < n; i++) shifted.t[i + num_ec_bytes] = ecp.t[i];

    poly_t gen = rs_generator(num_ec_bytes);
    poly_t rem = poly_remainder(shifted, gen);

    if (getenv("NE_QR_DEBUG")) {
        fprintf(stderr, "DBG rs_encode n=%d ec=%d\n", n, num_ec_bytes);
        fprintf(stderr, "DBG gen:"); for (int i = 0; i < gen.n; i++) fprintf(stderr, " %u", gen.t[i]);
        fprintf(stderr, "\nDBG rem:"); for (int i = 0; i < rem.n; i++) fprintf(stderr, " %u", rem.t[i]);
        fprintf(stderr, "\n");
    }

    *out = bts_clone(data);
    /* AppendBytes(remainder.data(numECBytes)): right-aligned to numECBytes */
    unsigned char *tail = calloc((size_t)num_ec_bytes, 1);
    int off = num_ec_bytes - rem.n;
    if (off < 0) off = 0;
    /* Go gfPoly.data(): writes e.term[len-1] (highest degree) into result[0],
     * i.e. highest-degree remainder coefficient first. In this C port the
     * polynomial is stored "reversed": rem.t[i] holds the HIGHEST-degree
     * coefficient when i is small (t[0] = x^(rem.n-1) coeff). So to emit the
     * high-degree coeff first (matching Go), walk rem.t from index 0 up —
     * NOT rem.t[rem.n-1] down. Verified: ref_ec == GO_EC, C output was its
     * exact byte-reverse, flipping this loop fixes it. */
    for (int j = 0; j < rem.n && off + j < num_ec_bytes; j++)
        tail[off + j] = rem.t[rem.n - 1 - j];
    for (int k = 0; k < num_ec_bytes; k++)
        bts_append_u32(out, tail[k], 8);
    free(tail);
    poly_free(&ecp); poly_free(&shifted); poly_free(&gen); poly_free(&rem);
}

/* ════════════════ symbol (symbol.go + regular_symbol.go) ════════════════ */
#define QUIET_ZONE 4
#define FMT_BITS 15
#define VER_BITS 18

static const int ALIGN_CENTER[42][8] = {
    {}, {}, {6,18}, {6,22}, {6,26}, {6,30}, {6,34}, {6,22,38}, {6,24,42},
    {6,26,46}, {6,28,50}, {6,30,54}, {6,32,58}, {6,34,62}, {6,26,46,66},
    {6,26,48,70}, {6,26,50,74}, {6,30,54,78}, {6,30,56,82}, {6,30,58,86},
    {6,34,62,90}, {6,28,50,72,94}, {6,26,50,74,98}, {6,30,54,78,102},
    {6,28,54,80,106}, {6,32,58,84,110}, {6,30,58,86,114}, {6,34,62,90,118},
    {6,26,50,74,98,122}, {6,30,54,78,102,126}, {6,26,52,78,104,130},
    {6,30,56,82,108,134}, {6,34,60,86,112,138}, {6,30,58,86,114,142},
    {6,34,62,90,118,146}, {6,30,54,78,102,126,150}, {6,24,50,76,102,128,154},
    {6,28,54,80,106,132,158}, {6,32,58,84,110,136,162}, {6,26,54,82,110,138,166},
    {6,30,58,86,114,142,170},
};
static const int ALIGN_CENTER_LEN[42] = {0,0,2,2,2,2,2,3,3,3,3,3,3,3,4,4,4,4,4,4,4,
    5,5,5,5,5,5,5,6,6,6,6,6,6,6,7,7,7,7,7,7};

static const uint32_t FMT_SEQ[32] = {
    0x5412,0x5125,0x5e7c,0x5b4b,0x45f9,0x40ce,0x4f97,0x4aa0,
    0x77c4,0x72f3,0x7daa,0x789d,0x662f,0x6318,0x6c41,0x6976,
    0x1689,0x13be,0x1ce7,0x19d0,0x0762,0x0255,0x0d0c,0x083b,
    0x355f,0x3068,0x3f31,0x3a06,0x24b4,0x2183,0x2eda,0x2bed,
};
static const uint32_t VER_SEQ[41] = {
    0,0,0,0,0,0,0, 0x07c94,0x085bc,0x09a99,0x0a4d3,0x0bbf6,0x0c762,0x0d847,
    0x0e60d,0x0f928,0x10b78,0x1145d,0x12a17,0x13532,0x149a6,0x15683,0x168c9,
    0x177ec,0x18ec4,0x191e1,0x1afab,0x1b08e,0x1cc1a,0x1d33f,0x1ed75,0x1f250,
    0x209d5,0x216f0,0x228ba,0x2379f,0x24b0b,0x2542e,0x26a64,0x27541,0x28c69,
};

typedef struct {
    int size, symbol_size, quiet;
    unsigned char *module, *used;
} sym_t;

static void sym_init(sym_t *m, int size, int quiet) {
    m->symbol_size = size;
    m->quiet = quiet;
    m->size = size + 2 * quiet;
    m->module = calloc((size_t)m->size * m->size, 1);
    m->used = calloc((size_t)m->size * m->size, 1);
}
static void sym_free(sym_t *m) { free(m->module); free(m->used); }
static int sym_get(const sym_t *m, int x, int y) {
    return m->module[(size_t)(y + m->quiet) * m->size + (x + m->quiet)];
}
static int sym_empty(const sym_t *m, int x, int y) {
    return !m->used[(size_t)(y + m->quiet) * m->size + (x + m->quiet)];
}
static void sym_set(sym_t *m, int x, int y, int v) {
    m->module[(size_t)(y + m->quiet) * m->size + (x + m->quiet)] = v ? 1 : 0;
    m->used[(size_t)(y + m->quiet) * m->size + (x + m->quiet)] = 1;
}
static void sym_set2d(sym_t *m, int x, int y, const char *pat, int rows, int cols) {
    for (int j = 0; j < rows; j++)
        for (int i = 0; i < cols; i++)
            sym_set(m, x + i, y + j, pat[j * cols + i]);
}

/* ── penalties (symbol.go, exact port) ── */
static int penalty1(const sym_t *m) {
    int p = 0;
    for (int x = 0; x < m->symbol_size; x++) {
        int last = sym_get(m, x, 0), count = 1;
        for (int y = 1; y < m->symbol_size; y++) {
            int v = sym_get(m, x, y);
            if (v != last) { count = 1; last = v; }
            else {
                count++;
                if (count == 6) p += 4;
                else if (count > 6) p++;
            }
        }
    }
    for (int y = 0; y < m->symbol_size; y++) {
        int last = sym_get(m, 0, y), count = 1;
        for (int x = 1; x < m->symbol_size; x++) {
            int v = sym_get(m, x, y);
            if (v != last) { count = 1; last = v; }
            else {
                count++;
                if (count == 6) p += 4;
                else if (count > 6) p++;
            }
        }
    }
    return p;
}
static int penalty2(const sym_t *m) {
    int p = 0;
    for (int y = 1; y < m->symbol_size; y++)
        for (int x = 1; x < m->symbol_size; x++) {
            int c = sym_get(m, x, y);
            if (c == sym_get(m, x - 1, y) && c == sym_get(m, x, y - 1) &&
                c == sym_get(m, x - 1, y - 1))
                p++;
        }
    return p * 3;
}
static int penalty3(const sym_t *m) {
    int p = 0;
    for (int y = 0; y < m->symbol_size; y++) {
        int buf = 0;
        for (int x = 0; x < m->symbol_size; x++) {
            buf <<= 1;
            if (sym_get(m, x, y)) buf |= 1;
            switch (buf & 0x7ff) {
                case 0x05d: case 0x5d0:
                    p += 40; buf = 0xFF;
                    break;
                default:
                    if (x == m->symbol_size - 1 && (buf & 0x7f) == 0x5d) {
                        p += 40; buf = 0xFF;
                    }
            }
        }
    }
    for (int x = 0; x < m->symbol_size; x++) {
        int buf = 0;
        for (int y = 0; y < m->symbol_size; y++) {
            buf <<= 1;
            if (sym_get(m, x, y)) buf |= 1;
            switch (buf & 0x7ff) {
                case 0x05d: case 0x5d0:
                    p += 40; buf = 0xFF;
                    break;
                default:
                    if (y == m->symbol_size - 1 && (buf & 0x7f) == 0x5d) {
                        p += 40; buf = 0xFF;
                    }
            }
        }
    }
    return p;
}
static int penalty4(const sym_t *m) {
    int num = m->symbol_size * m->symbol_size;
    int dark = 0;
    for (int x = 0; x < m->symbol_size; x++)
        for (int y = 0; y < m->symbol_size; y++)
            if (sym_get(m, x, y)) dark++;
    int dev = num / 2 - dark;
    if (dev < 0) dev = -dev;
    return 10 * (dev / (num / 20));
}

/* buildRegularSymbol(version, mask, data, includeQuietZone) */
static int build_symbol(const ne_qr_version *v, int mask, const bits_t *data,
                        int include_quiet, sym_t *out) {
    int size = 21 + (v->version - 1) * 4;
    int quiet = include_quiet ? QUIET_ZONE : 0;
    sym_init(out, size, quiet);

    /* finder patterns (3 corners + separators) */
    static const char FP[49] = {
        1,1,1,1,1,1,1,
        1,0,0,0,0,0,1,
        1,0,1,1,1,0,1,
        1,0,1,1,1,0,1,
        1,0,1,1,1,0,1,
        1,0,0,0,0,0,1,
        1,1,1,1,1,1,1,
    };
    static const char FPH[8] = {0,0,0,0,0,0,0,0};
    static const char FPV[8] = {0,0,0,0,0,0,0,0};
    sym_set2d(out, 0, 0, FP, 7, 7);
    sym_set2d(out, 0, 7, FPH, 1, 8);
    sym_set2d(out, 7, 0, FPV, 8, 1);
    sym_set2d(out, size - 7, 0, FP, 7, 7);
    sym_set2d(out, size - 8, 7, FPH, 1, 8);
    sym_set2d(out, size - 8, 0, FPV, 8, 1);
    sym_set2d(out, 0, size - 7, FP, 7, 7);
    sym_set2d(out, 0, size - 8, FPH, 1, 8);
    sym_set2d(out, 7, size - 8, FPV, 8, 1);

    /* alignment patterns */
    static const char AP[25] = {
        1,1,1,1,1,
        1,0,0,0,1,
        1,0,1,0,1,
        1,0,0,0,1,
        1,1,1,1,1,
    };
    for (int ai = 0; ai < ALIGN_CENTER_LEN[v->version]; ai++)
        for (int aj = 0; aj < ALIGN_CENTER_LEN[v->version]; aj++) {
            int x = ALIGN_CENTER[v->version][ai], y = ALIGN_CENTER[v->version][aj];
            if (!sym_empty(out, x, y)) continue;
            sym_set2d(out, x - 2, y - 2, AP, 5, 5);
        }

    /* timing patterns */
    int value = 1;
    for (int i = 8; i < size - 7; i++) {
        sym_set(out, i, 6, value);
        sym_set(out, 6, i, value);
        value = !value;
    }

    /* format info */
    int fmt_id = mask & 0x7; /* Medium => level bits 0b000 */
    uint32_t f = FMT_SEQ[fmt_id];
    /* Go writes f.At(l-i) with i ascending; its Bitset stores MSB-first but
     * AppendUint32(15) lands bit14..bit0 at positions 0..14, so At(l-i) walks
     * bit0 first.  Use (f >> i) & 1 to match (zbar reads this layout). */
    for (int i = 0; i <= 7; i++) sym_set(out, size - i - 1, 8, (f >> i) & 1);
    for (int i = 0; i <= 5; i++) sym_set(out, 8, i, (f >> i) & 1);
    sym_set(out, 8, 7, (f >> 6) & 1);
    sym_set(out, 8, 8, (f >> 7) & 1);
    sym_set(out, 7, 8, (f >> 8) & 1);
    for (int i = 9; i <= 14; i++) sym_set(out, 14 - i, 8, (f >> i) & 1);
    for (int i = 8; i <= 14; i++)
        sym_set(out, 8, size - 7 + i - 8, (f >> i) & 1);
    sym_set(out, 8, size - 8, 1); /* dark module */

    /* version info (v7+) */
    if (v->version >= 7) {
        uint32_t vi = VER_SEQ[v->version];
        /* Go versionInfo(): v.At(l-i) again walks bit0 first. */
        for (int i = 0; i < VER_BITS; i++) {
            sym_set(out, i / 3, size - 11 + i % 3, (vi >> i) & 1);
            sym_set(out, size - 11 + i % 3, i / 3, (vi >> i) & 1);
        }
    }

    /* data placement (addData) */
    if (getenv("NE_QR_DEBUG") && getenv("NE_QR_DUMP")) {
        char pp[600];
        snprintf(pp, sizeof pp, "%s/func_v%d.txt", getenv("NE_QR_DUMP"), v->version);
        FILE *uf = fopen(pp, "w");
        if (uf) {
            for (int yy = 0; yy < size; yy++) {
                for (int xx2 = 0; xx2 < size; xx2++)
                    fputc(out->used[(size_t)(yy + out->quiet) * out->size + xx2 + out->quiet] ? 'u' : '.', uf);
                fputc('\n', uf);
            }
            fclose(uf);
        }
    }
    int x_off = 1, dir_up = 1;
    int x = size - 2, y = size - 1;
    FILE *walk_f = NULL;
    if (getenv("NE_QR_DUMP")) {
        char wp[600];
        snprintf(wp, sizeof wp, "%s/walk_v%d.txt", getenv("NE_QR_DUMP"), v->version);
        walk_f = fopen(wp, "w");
    }
    for (size_t i = 0; i < data->len; i++) {
        int xx = x + x_off;
        if (walk_f) fprintf(walk_f, "%d %d\n", xx, y);
        int m;
        switch (mask) {
            case 0: m = (y + xx) % 2 == 0; break;
            case 1: m = y % 2 == 0; break;
            case 2: m = xx % 3 == 0; break;
            case 3: m = (y + xx) % 3 == 0; break;
            case 4: m = (y / 2 + xx / 3) % 2 == 0; break;
            case 5: m = (y * xx) % 2 + (y * xx) % 3 == 0; break;
            case 6: m = ((y * xx) % 2 + (y * xx) % 3) % 2 == 0; break;
            default: m = ((y + xx) % 2 + (y * xx) % 3) % 2 == 0; break;
        }
        sym_set(out, xx, y, m != bts_at(data, i));

        if (i == data->len - 1) break;
        for (;;) {
            if (x_off == 1) {
                x_off = 0;
            } else {
                x_off = 1;
                if (dir_up) {
                    if (y > 0) y--;
                    else { dir_up = 0; x -= 2; }
                } else {
                    if (y < size - 1) y++;
                    else { dir_up = 1; x -= 2; }
                }
            }
            if (x == 5) x--;
            if (sym_empty(out, x + x_off, y)) break;
        }
    }
    if (walk_f) fclose(walk_f);

    /* numEmptyModules must be 0 (Go panics otherwise) */
    int empty = 0;
    for (int yy = 0; yy < size; yy++)
        for (int xx2 = 0; xx2 < size; xx2++)
            if (sym_empty(out, xx2, yy)) empty++;
    if (empty != 0 && getenv("NE_QR_DEBUG")) {
        fprintf(stderr, "empty=%d size=%d v%d first:", empty, size, v->version);
        const char *dp = getenv("NE_QR_DUMP");
        if (dp && *dp) {
            char pp[600];
            snprintf(pp, sizeof pp, "%s/used_v%d_m%d.txt", dp, v->version, mask);
            FILE *uf = fopen(pp, "w");
            if (uf) {
                for (int yy = 0; yy < size; yy++) {
                    for (int xx2 = 0; xx2 < size; xx2++)
                        fputc(out->used[(size_t)(yy + out->quiet) * out->size + xx2 + out->quiet] ? 'u' : '.', uf);
                    fputc('\n', uf);
                }
                fclose(uf);
            }
        }
        int shown = 0;
        for (int yy = 0; yy < size && shown < 12; yy++)
            for (int xx2 = 0; xx2 < size && shown < 12; xx2++)
                if (sym_empty(out, xx2, yy)) { fprintf(stderr, " (%d,%d)", xx2, yy); shown++; }
        fprintf(stderr, "\n");
    }
    if (empty != 0) { sym_free(out); return -1; }
    return 0;
}

/* ════════════════ public pipeline (qrcode.go New/encode) ════════════════ */
static const ne_qr_version *choose_version(const enc_type_t *enc, long num_bits) {
    for (int i = 0; i < 40; i++) {
        const ne_qr_version *v = &ne_qr_versions_medium[i];
        if (v->version < enc->min_version) continue;
        if (v->version > enc->max_version) break;
        long data_bits = 0;
        for (int b = 0; b < v->num_block_specs; b++)
            data_bits += 8L * v->block[b].num_blocks * v->block[b].num_data_codewords;
        if (data_bits - num_bits >= 0) return v;
    }
    return NULL;
}
static long version_data_bits(const ne_qr_version *v) {
    long n = 0;
    for (int b = 0; b < v->num_block_specs; b++)
        n += 8L * v->block[b].num_blocks * v->block[b].num_data_codewords;
    return n;
}

ne_qr *ne_qr_new(const char *content, const char **err_out) {
    const enc_type_t *encs[3] = {&ENC_1_9, &ENC_10_26, &ENC_27_40};
    bits_t encoded;
    const ne_qr_version *chosen = NULL;
    const char *last_err = NULL;

    size_t n = content ? strlen(content) : 0;
    const unsigned char *bytes = (const unsigned char *)content;

    for (int e = 0; e < 3; e++) {
        const char *eerr = NULL;
        if (data_encode(encs[e], bytes, n, &encoded, &eerr) != 0) {
            last_err = eerr; /* Go: err overwritten each iteration */
            continue;
        }
        last_err = NULL;
        chosen = choose_version(encs[e], (long)encoded.len);
        if (chosen) break;
        bts_free(&encoded);
    }
    if (!chosen) {
        if (err_out) *err_out = last_err ? last_err : "content too long to encode";
        return NULL;
    }
    if (getenv("NE_QR_DEBUG"))
        fprintf(stderr, "DBG enc: n=%zu encoded=%zu -> v%d\n", n, encoded.len,
                chosen->version);

    /* terminator + padding */
    long free_bits = version_data_bits(chosen) - (long)encoded.len;
    long term = free_bits >= 4 ? 4 : free_bits;
    bts_append_false(&encoded, (size_t)term);

    long num_data_bits = version_data_bits(chosen);
    if ((long)encoded.len != num_data_bits) {
        bts_append_false(&encoded, (size_t)((8 - (long)encoded.len % 8) % 8));
        int i = 0;
        while (num_data_bits - (long)encoded.len >= 8) {
            /* Go addPadding: 0b11101100 (0xEC) then 0b00010001 (0x11),
             * alternating. (0xED is a common mistake — 0x11 is correct.) */
            unsigned char pad = i == 0 ? 0xEC : 0x11;
            bts_append_u32(&encoded, pad, 8);
            i = 1 - i;
        }
    }

    /* encodeBlocks: per-block RS + interleave */
    if (getenv("NE_QR_DEBUG")) {
        fprintf(stderr, "DBG encdata: ");
        for (size_t k = 0; k + 8 <= encoded.len; k += 8) {
            unsigned char b = 0;
            for (int bb = 0; bb < 8; bb++) b = (unsigned char)((b << 1) | bts_at(&encoded, k + bb));
            fprintf(stderr, "%02x", b);
        }
        fprintf(stderr, "\n");
    }
    bits_t blk[128];
    int ec_off[128], blk_len[128], nblk = 0;
    long start = 0, end = 0;
    for (int b = 0; b < chosen->num_block_specs; b++)
        for (int j = 0; j < chosen->block[b].num_blocks; j++) {
            start = end;
            end = start + 8L * chosen->block[b].num_data_codewords;
            int num_ec = chosen->block[b].num_codewords - chosen->block[b].num_data_codewords;
            bits_t sub = bts_substr(&encoded, (size_t)start, (size_t)end);
            rs_encode(&sub, num_ec, &blk[nblk]);
            bts_free(&sub);
            ec_off[nblk] = (int)(end - start);
            blk_len[nblk] = (int)blk[nblk].len;
            nblk++;
        }

    bits_t final_data;
    bts_init(&final_data);
    int working = 1;
    for (long i = 0; working; i += 8) {
        working = 0;
        for (int j = 0; j < nblk; j++) {
            if (i >= ec_off[j]) continue;
            bits_t s = bts_substr(&blk[j], (size_t)i, (size_t)(i + 8));
            bts_append_bits(&final_data, s.b, s.len);
            bts_free(&s);
            working = 1;
        }
    }
    working = 1;
    for (long i = 0; working; i += 8) {
        working = 0;
        for (int j = 0; j < nblk; j++) {
            long off = i + ec_off[j];
            if (off >= blk_len[j]) continue;
            bits_t s = bts_substr(&blk[j], (size_t)off, (size_t)(off + 8));
            bts_append_bits(&final_data, s.b, s.len);
            bts_free(&s);
            working = 1;
        }
    }
    bts_append_false(&final_data, (size_t)chosen->remainder_bits);
    if (getenv("NE_QR_DEBUG")) {
        long total_cw = 0;
        for (int b = 0; b < chosen->num_block_specs; b++)
            for (int j = 0; j < chosen->block[b].num_blocks; j++)
                total_cw += chosen->block[b].num_codewords;
        fprintf(stderr, "DBG v%d nblk=%d databits=%ld final=%zu expect=%ld rem=%d\n",
                chosen->version, nblk, num_data_bits, final_data.len,
                8 * total_cw + chosen->remainder_bits, chosen->remainder_bits);
        /* dump the interleaved final stream (codewords, hex) */
        fprintf(stderr, "DBG finalstream: ");
        for (size_t k = 0; k + 8 <= final_data.len; k += 8) {
            unsigned char b = 0;
            for (int bb = 0; bb < 8; bb++) b = (unsigned char)((b << 1) | bts_at(&final_data, k + bb));
            fprintf(stderr, "%02x", b);
        }
        fprintf(stderr, "\n");
    }
    for (int j = 0; j < nblk; j++) bts_free(&blk[j]);
    bts_free(&encoded);

    /* mask selection: first-lowest penalty wins (Go: p < penalty) */
    ne_qr *q = NULL;
    int best_penalty = 0;
    for (int mask = 0; mask < 8; mask++) {
        sym_t s;
        if (build_symbol(chosen, mask, &final_data, 1, &s) != 0) {
            bts_free(&final_data);
            if (err_out) *err_out = "bug: numEmptyModules is not 0";
            return NULL;
        }
        int p = penalty1(&s) + penalty2(&s) + penalty3(&s) + penalty4(&s);
        if (getenv("NE_QR_DEBUG")) fprintf(stderr, "DBG mask%d penalty=%d\n", mask, p);
        if (!q || p < best_penalty) {
            if (q) free(q->bits), free(q);
            q = malloc(sizeof *q);
            q->size = s.size;
            q->version = chosen->version;
            q->mask = mask;
            q->bits = malloc((size_t)s.size * s.size);
            memcpy(q->bits, s.module, (size_t)s.size * s.size);
            best_penalty = p;
        }
        sym_free(&s);
    }
    bts_free(&final_data);
    if (q && getenv("NE_QR_DEBUG")) {
        fprintf(stderr, "DBG final mask=%d size=%d\n", q->mask, q->size);
        for (int yy = 0; yy < q->size; yy++) {
            fputs("  ", stderr);
            for (int xx2 = 0; xx2 < q->size; xx2++)
                fputc(q->bits[(size_t)yy * q->size + xx2] ? '#' : '.', stderr);
            fputc('\n', stderr);
        }
    }
    return q;
}

void ne_qr_free(ne_qr *q) {
    if (!q) return;
    free(q->bits);
    free(q);
}

char *ne_qr_small_string(const ne_qr *q) {
    /* ToSmallString(false): dark module -> ' ' / light -> '█';
     * upper-half dark -> '▄', lower-half dark -> '▀' (inverse=false) */
    size_t cap = (size_t)q->size * ((size_t)q->size / 2 + 2) * 3 + 16;
    char *out = malloc(cap);
    size_t o = 0;
    for (int y = 0; y < q->size - 1; y += 2) {
        for (int x = 0; x < q->size; x++) {
            int up = q->bits[(size_t)y * q->size + x];
            int dn = q->bits[(size_t)(y + 1) * q->size + x];
            if (up == dn) {
                if (up) out[o++] = ' ';
                else { out[o++] = (char)0xE2; out[o++] = (char)0x96; out[o++] = (char)0x88; }
            } else if (up) { /* upper dark */
                out[o++] = (char)0xE2; out[o++] = (char)0x96; out[o++] = (char)0x84;
            } else { /* lower dark */
                out[o++] = (char)0xE2; out[o++] = (char)0x96; out[o++] = (char)0x80;
            }
        }
        out[o++] = '\n';
    }
    if (q->size % 2 == 1) {
        int y = q->size - 1;
        for (int x = 0; x < q->size; x++) {
            /* Go ToSmallString last-row: dark -> ' ', light -> '▀' */
            if (q->bits[(size_t)y * q->size + x]) out[o++] = ' ';
            else { out[o++] = (char)0xE2; out[o++] = (char)0x96; out[o++] = (char)0x80; }
        }
        out[o++] = '\n';
    }
    out[o] = '\0';
    return out;
}

/* ════════════════ PNG writer ════════════════ */
static void put_be32(unsigned char *p, uint32_t v) {
    p[0] = (unsigned char)(v >> 24); p[1] = (unsigned char)(v >> 16);
    p[2] = (unsigned char)(v >> 8);  p[3] = (unsigned char)v;
}
static uint32_t crc32_raw(const unsigned char *d, size_t n) {
#ifdef NE_HAVE_ZLIB
    return (uint32_t)crc32(0L, d, (uInt)n);
#else
    static uint32_t tbl[256]; static int ready = 0;
    if (!ready) {
        for (uint32_t i = 0; i < 256; i++) {
            uint32_t c = i;
            for (int k = 0; k < 8; k++) c = (c & 1) ? 0xEDB88320u ^ (c >> 1) : c >> 1;
            tbl[i] = c;
        }
        ready = 1;
    }
    uint32_t c = 0xFFFFFFFFu;
    for (size_t i = 0; i < n; i++) c = tbl[(c ^ d[i]) & 0xFF] ^ (c >> 8);
    return c ^ 0xFFFFFFFFu;
#endif
}
static uint32_t adler32_raw(const unsigned char *d, size_t n) {
    uint32_t a = 1, b = 0;
    for (size_t i = 0; i < n; i++) {
        a = (a + d[i]) % 65521;
        b = (b + a) % 65521;
    }
    return (b << 16) | a;
}

typedef struct { unsigned char *d; size_t len, cap; } buf_t;
static void buf_put(buf_t *b, const void *d, size_t n) {
    if (b->len + n > b->cap) {
        size_t nc = b->cap ? b->cap * 2 : 256;
        while (nc < b->len + n) nc *= 2;
        b->d = realloc(b->d, nc);
        b->cap = nc;
    }
    memcpy(b->d + b->len, d, n);
    b->len += n;
}
static void png_chunk(buf_t *out, const char *type, const unsigned char *data, size_t n) {
    unsigned char hdr[8], crcb[4];
    put_be32(hdr, (uint32_t)n);
    buf_put(out, hdr, 4);
    unsigned char *t = malloc(n + 4);
    memcpy(t, type, 4);
    if (n) memcpy(t + 4, data, n);
    buf_put(out, t, n + 4);
    put_be32(crcb, crc32_raw(t, n + 4));
    buf_put(out, crcb, 4);
    free(t);
}

/* adaptive filter, same heuristic as image/png (min sum of |int8|) */
static int png_filter_row(unsigned char *row, const unsigned char *prior,
                          int w, unsigned char *out) {
    unsigned char cand[5][480];
    int sums[5] = {0};
    for (int f = 0; f < 5; f++) {
        for (int i = 0; i < w; i++) {
            int x = row[i];
            int a = i > 0 ? row[i - 1] : 0;
            int b = prior ? prior[i] : 0;
            int c = (i > 0 && prior) ? prior[i - 1] : 0;
            int v;
            switch (f) {
                case 0: v = x; break;
                case 1: v = x - a; break;
                case 2: v = x - b; break;
                case 3: v = x - ((a + b) / 2); break;
                case 4: { /* Paeth */
                    int p = a + b - c, pa = abs(p - a), pb = abs(p - b), pc = abs(p - c);
                    int pr = (pa <= pb && pa <= pc) ? a : (pb <= pc ? b : c);
                    v = x - pr;
                } break;
                default: v = x; break;
            }
            cand[f][i] = (unsigned char)v;
            int sv = (int8_t)v; /* signed-byte abs like Go */
            sums[f] += sv < 0 ? -sv : sv;
        }
    }
    int best = 0;
    for (int f = 1; f < 5; f++)
        if (sums[f] < sums[best]) best = f;
    out[0] = (unsigned char)best;
    memcpy(out + 1, cand[best], (size_t)w);
    return 0;
}

unsigned char *ne_qr_png(const ne_qr *q, int size, size_t *out_len) {
    int real = q->size;
    if (size < real) size = real;
    double mpp = (double)real / (double)size;

    if (getenv("NE_QR_DEBUG"))
        fprintf(stderr, "DBG png real=%d q->size=%d arg_size=%d mpp=%f\n",
                real, q->size, size, mpp);

    /* raw scanlines: filter byte + `size` bytes (palette index 1 = black) */
    size_t raw_len = (size_t)size * (size + 1);
    unsigned char *raw = malloc(raw_len);
    unsigned char *cur = malloc((size_t)size);
    unsigned char *prior = calloc((size_t)size, 1);
    for (int y = 0; y < size; y++) {
        int y2 = (int)((double)y * mpp);
        for (int x = 0; x < size; x++) {
            int x2 = (int)((double)x * mpp);
            cur[x] = q->bits[(size_t)y2 * q->size + x2] ? 1 : 0;
        }
        png_filter_row(cur, prior, size, raw + (size_t)y * (size + 1));
        memcpy(prior, cur, (size_t)size);
    }
    free(cur); free(prior);

    /* zlib stream: level 9 via libz, else stored deflate blocks */
    buf_t z; z.d = NULL; z.len = z.cap = 0;
#ifdef NE_HAVE_ZLIB
    uLongf zlen = compressBound(raw_len);
    unsigned char *zbuf = malloc(zlen);
    if (compress2(zbuf, &zlen, raw, (uLong)raw_len, 9) == Z_OK) {
        buf_put(&z, zbuf, zlen);
        free(zbuf);
    } else {
        free(zbuf);
#endif
        /* fallback: stored (uncompressed) deflate — still a valid PNG */
        unsigned char two[2] = {0x78, 0x01};
        buf_put(&z, two, 2);
        size_t off = 0;
        while (off < raw_len) {
            size_t chunk = raw_len - off;
            if (chunk > 65535) chunk = 65535;
            unsigned char hdr[5];
            int last = (off + chunk >= raw_len);
            hdr[0] = last ? 1 : 0;
            hdr[1] = (unsigned char)(chunk & 0xFF);
            hdr[2] = (unsigned char)(chunk >> 8);
            hdr[3] = (unsigned char)(~chunk & 0xFF);
            hdr[4] = (unsigned char)((~chunk >> 8) & 0xFF);
            buf_put(&z, hdr, 5);
            buf_put(&z, raw + off, chunk);
            off += chunk;
        }
        unsigned char adler[4];
        uint32_t ad = adler32_raw(raw, raw_len);
        put_be32(adler, ad);
        buf_put(&z, adler, 4);
#ifdef NE_HAVE_ZLIB
    }
#endif
    free(raw);

    buf_t out; out.d = NULL; out.len = out.cap = 0;
    unsigned char sig[8] = {137, 80, 78, 71, 13, 10, 26, 10};
    buf_put(&out, sig, 8);

    unsigned char ihdr[13];
    put_be32(ihdr, (uint32_t)size);
    put_be32(ihdr + 4, (uint32_t)size);
    ihdr[8] = 8;  /* bit depth */
    ihdr[9] = 3;  /* colour type: palette */
    ihdr[10] = 0; ihdr[11] = 0; ihdr[12] = 0;
    png_chunk(&out, "IHDR", ihdr, 13);

    unsigned char plte[6] = {255, 255, 255, 0, 0, 0}; /* white bg, black fg */
    png_chunk(&out, "PLTE", plte, 6);
    png_chunk(&out, "IDAT", z.d, z.len);
    png_chunk(&out, "IEND", NULL, 0);
    free(z.d);

    if (out_len) *out_len = out.len;
    return out.d;
}
