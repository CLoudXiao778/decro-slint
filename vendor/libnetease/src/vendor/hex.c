#include "netease/encoding.h"
#include "netease/util.h"

static const char hex_lower_tab[] = "0123456789abcdef";
static const char hex_upper_tab[] = "0123456789ABCDEF";

static char *hex_impl(const uint8_t *in, size_t n, const char *tab) {
    char *out = ne_xmalloc(n * 2 + 1);
    for (size_t i = 0; i < n; i++) {
        out[i * 2] = tab[in[i] >> 4];
        out[i * 2 + 1] = tab[in[i] & 15];
    }
    out[n * 2] = '\0';
    return out;
}

char *ne_hex_lower(const uint8_t *in, size_t n) { return hex_impl(in, n, hex_lower_tab); }
char *ne_hex_upper(const uint8_t *in, size_t n) { return hex_impl(in, n, hex_upper_tab); }
