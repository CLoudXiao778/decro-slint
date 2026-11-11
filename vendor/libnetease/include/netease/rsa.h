#ifndef NE_VENDOR_RSA_H
#define NE_VENDOR_RSA_H
#include <stddef.h>
#include <stdint.h>
#include "bignum.h"

/* Load the netease public key (the v1.6.0 PEM embedded from cryto.go) into
 * (n, e). Returns 0 on success. DER SubjectPublicKeyInfo is walked manually —
 * no external TLS library needed. */
int ne_rsa_load_key(bn *n, uint32_t *e);

/* Mirror of cryto.go rsaEncrypt():
 *   - the 16-byte secretKey is left-padded with ZERO bytes to 128 bytes
 *   - c = buf ^ e (mod n)          — textbook RSA, NO padding
 *   - output = big.Int.Bytes()     — leading zero bytes are STRIPPED
 * out must have capacity 128; returns byte count (<= 128). */
size_t ne_rsa_encrypt_secretkey(const uint8_t secret_key[16],
                                const bn *n, uint32_t e,
                                uint8_t *out);
#endif
