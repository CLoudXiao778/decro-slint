/* Minimal unsigned bignum — enough for 1024-bit textbook RSA. */
#include "netease/bignum.h"
#include <string.h>

void bn_zero(bn *a) { memset(a->d, 0, sizeof(a->d)); a->n = 0; }

int bn_from_be(bn *a, const uint8_t *buf, size_t len) {
    if (len > (size_t)BN_LIMBS * 4) return -1;
    bn_zero(a);
    size_t i = len;
    int limb = 0;
    while (i > 0) {
        size_t start = (i >= 4) ? i - 4 : 0;
        uint32_t v = 0;
        for (size_t k = start; k < i; k++) v = (v << 8) | buf[k];
        a->d[limb++] = v;
        i = start;
    }
    while (limb > 0 && a->d[limb - 1] == 0) limb--;
    a->n = limb;
    return 0;
}

size_t bn_to_be_stripped(const bn *a, uint8_t *out) {
    if (a->n == 0) return 0;
    uint8_t full[BN_LIMBS * 4];
    size_t w = 0;
    /* most significant limb first — big-endian byte string */
    for (int i = a->n - 1; i >= 0; i--) {
        full[w++] = (uint8_t)(a->d[i] >> 24);
        full[w++] = (uint8_t)(a->d[i] >> 16);
        full[w++] = (uint8_t)(a->d[i] >> 8);
        full[w++] = (uint8_t)(a->d[i]);
    }
    size_t lead = 0;
    while (lead < w && full[lead] == 0) lead++;   /* Go big.Int.Bytes() */
    memcpy(out, full + lead, w - lead);
    return w - lead;
}

int bn_cmp(const bn *a, const bn *b) {
    if (a->n != b->n) return a->n > b->n ? 1 : -1;
    for (int i = a->n - 1; i >= 0; i--) {
        if (a->d[i] != b->d[i]) return a->d[i] > b->d[i] ? 1 : -1;
    }
    return 0;
}

void bn_sub(bn *a, const bn *b) {   /* a -= b, requires a >= b */
    uint64_t borrow = 0;
    for (int i = 0; i < a->n; i++) {
        uint64_t bv = (i < b->n) ? b->d[i] : 0;
        uint64_t v = (uint64_t)a->d[i] - bv - borrow;
        a->d[i] = (uint32_t)v;
        borrow = (v >> 63) & 1;
    }
    int n = a->n;
    while (n > 0 && a->d[n - 1] == 0) n--;
    a->n = n;
}

void bn_shl1(bn *a) {
    uint32_t carry = 0;
    for (int i = 0; i < a->n; i++) {
        uint32_t next = a->d[i] >> 31;
        a->d[i] = (a->d[i] << 1) | carry;
        carry = next;
    }
    if (carry && a->n < BN_LIMBS) a->d[a->n++] = carry;
}

void bn_mod(bn *a, const bn *m) {   /* a %= m, binary long division */
    if (bn_cmp(a, m) < 0) return;
    bn rem;
    bn_zero(&rem);
    for (int i = a->n * 32 - 1; i >= 0; i--) {
        bn_shl1(&rem);
        uint32_t bit = (a->d[i / 32] >> (i % 32)) & 1;
        if (rem.n == 0) {
            if (bit) { rem.d[0] = 1; rem.n = 1; }
        } else {
            rem.d[0] |= bit;
        }
        if (bn_cmp(&rem, m) >= 0) bn_sub(&rem, m);
    }
    *a = rem;
}

void bn_mulmod(bn *r, const bn *a, const bn *b, const bn *m) {
    /* schoolbook product into a double-width limb buffer.
     * Our RSA operands are < 1024 bits (32 limbs), so the product fits in
     * 64 limbs < BN_LIMBS. The assertions of shape are guaranteed by callers. */
    uint32_t acc[BN_LIMBS * 2];
    memset(acc, 0, sizeof(acc));
    for (int i = 0; i < a->n; i++) {
        uint64_t carry = 0;
        for (int j = 0; j < b->n; j++) {
            uint64_t cur = acc[i + j] + (uint64_t)a->d[i] * b->d[j] + carry;
            acc[i + j] = (uint32_t)cur;
            carry = cur >> 32;
        }
        acc[i + b->n] = (uint32_t)((uint64_t)acc[i + b->n] + carry);
    }
    int pn = a->n + b->n;
    while (pn > 0 && acc[pn - 1] == 0) pn--;

    bn p;
    bn_zero(&p);
    for (int i = 0; i < pn; i++) p.d[i] = acc[i];
    p.n = pn;
    bn_mod(&p, m);
    *r = p;
}
