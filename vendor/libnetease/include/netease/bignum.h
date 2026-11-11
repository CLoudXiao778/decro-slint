#ifndef NE_VENDOR_BIGNUM_H
#define NE_VENDOR_BIGNUM_H
#include <stddef.h>
#include <stdint.h>

/* Minimal fixed-capacity unsigned bignum — sized for the netease RSA:
 * 1024-bit modulus, 2048-bit intermediate products. */

#define BN_LIMBS 68   /* 68 * 32 = 2176 bits > 2048 */

typedef struct { uint32_t d[BN_LIMBS]; int n; } bn;  /* n = significant limbs (0 means zero) */

void     bn_zero(bn *a);
/* big-endian bytes -> bn; returns 0 on success, -1 if longer than BN_LIMBS*4 */
int      bn_from_be(bn *a, const uint8_t *buf, size_t len);
/* bn -> big-endian bytes with leading zero bytes STRIPPED (mirror of Go big.Int.Bytes()).
 * out capacity must be >= BN_LIMBS*4; returns byte count. */
size_t   bn_to_be_stripped(const bn *a, uint8_t *out);
int      bn_cmp(const bn *a, const bn *b);
/* a -= b (assumes a >= b) */
void     bn_sub(bn *a, const bn *b);
/* a = a << 1 */
void     bn_shl1(bn *a);
/* a %= m via binary long division */
void     bn_mod(bn *a, const bn *m);
/* r = (a * b) mod m */
void     bn_mulmod(bn *r, const bn *a, const bn *b, const bn *m);
#endif
