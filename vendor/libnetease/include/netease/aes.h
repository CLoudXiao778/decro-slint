#ifndef NE_VENDOR_AES_H
#define NE_VENDOR_AES_H
#include <stddef.h>
#include <stdint.h>

/* AES-128 encryption only — that is all weapi/eapi need.
 * The S-box and round constants are DERIVED at first use (GF(2^8) inverse +
 * affine transform), not transcribed, so there is no table to mistype. */

typedef struct {
    uint8_t round_keys[176];   /* 44 words as bytes, big-endian per word */
} ne_aes128;

void ne_aes128_init(ne_aes128 *ctx, const uint8_t key[16]);
void ne_aes128_encrypt_block(const ne_aes128 *ctx, const uint8_t in[16], uint8_t out[16]);

/* PKCS#7-padded CBC / ECB encryption (mirror of openssl.AesCBCEncrypt /
 * AesECBEncrypt with PKCS7_PADDING, as used by cryto.go).
 * Returns malloc'd ciphertext, size stored in *out_len. */
uint8_t *ne_aes_cbc_encrypt(const uint8_t *in, size_t n,
                            const uint8_t key[16], const uint8_t iv[16],
                            size_t *out_len);
uint8_t *ne_aes_ecb_encrypt(const uint8_t *in, size_t n,
                            const uint8_t key[16], size_t *out_len);
#endif
