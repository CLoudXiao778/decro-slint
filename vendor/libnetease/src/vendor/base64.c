#include "netease/encoding.h"
#include "netease/util.h"
#include <string.h>

static const char b64_tab[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

char *ne_base64_encode(const uint8_t *in, size_t n) {
    size_t out_len = ((n + 2) / 3) * 4;
    char *out = ne_xmalloc(out_len + 1);
    size_t i, o = 0;
    for (i = 0; i + 2 < n; i += 3) {
        uint32_t v = ((uint32_t)in[i] << 16) | ((uint32_t)in[i + 1] << 8) | in[i + 2];
        out[o++] = b64_tab[(v >> 18) & 63];
        out[o++] = b64_tab[(v >> 12) & 63];
        out[o++] = b64_tab[(v >> 6) & 63];
        out[o++] = b64_tab[v & 63];
    }
    if (i < n) {
        uint32_t v = (uint32_t)in[i] << 16;
        int rem = (int)(n - i);
        if (rem == 2) v |= (uint32_t)in[i + 1] << 8;
        out[o++] = b64_tab[(v >> 18) & 63];
        out[o++] = b64_tab[(v >> 12) & 63];
        out[o++] = (rem == 2) ? b64_tab[(v >> 6) & 63] : '=';
        out[o++] = '=';
    }
    out[o] = '\0';
    return out;
}

static int b64_val(char c) {
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == '+') return 62;
    if (c == '/') return 63;
    return -1;
}

uint8_t *ne_base64_decode(const char *in, size_t *out_len) {
    size_t n = strlen(in);
    while (n > 0 && in[n - 1] == '=') n--;
    uint8_t *out = ne_xmalloc((n / 4) * 3 + 3);
    size_t o = 0, i = 0;
    while (i + 3 < n + 1 && i < n) {
        int a = b64_val(in[i]);
        int b = (i + 1 < n) ? b64_val(in[i + 1]) : -1;
        int c = (i + 2 < n) ? b64_val(in[i + 2]) : -1;
        int d = (i + 3 < n) ? b64_val(in[i + 3]) : -1;
        if (a < 0 || b < 0) { free(out); if (out_len) *out_len = 0; return NULL; }
        uint32_t v = ((uint32_t)a << 18) | ((uint32_t)b << 12);
        out[o++] = (uint8_t)(v >> 16);
        if (c >= 0) { v |= (uint32_t)c << 6; out[o++] = (uint8_t)(v >> 8); }
        if (d >= 0) { v |= (uint32_t)d; out[o++] = (uint8_t)v; }
        i += 4;
    }
    out[o] = 0;
    if (out_len) *out_len = o;
    return out;
}
