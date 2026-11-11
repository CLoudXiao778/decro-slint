#ifndef NE_VENDOR_B64HEX_H
#define NE_VENDOR_B64HEX_H
#include <stddef.h>
#include <stdint.h>

/* RFC 4648 standard base64 with padding (Go base64.StdEncoding) */
char *ne_base64_encode(const uint8_t *in, size_t n);          /* malloc'd, NUL-terminated */
uint8_t *ne_base64_decode(const char *in, size_t *out_len);   /* malloc'd */

/* hex, lowercase / uppercase variants (Go hex.EncodeToString / strings.ToUpper) */
char *ne_hex_lower(const uint8_t *in, size_t n);
char *ne_hex_upper(const uint8_t *in, size_t n);
#endif
