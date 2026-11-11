#ifndef NE_VENDOR_MD5_H
#define NE_VENDOR_MD5_H
#include <stddef.h>
#include <stdint.h>

typedef struct {
    uint32_t h[4];
    uint64_t len;          /* total bytes hashed */
    uint8_t  buf[64];
    size_t   buflen;
} ne_md5;

void ne_md5_init(ne_md5 *c);
void ne_md5_update(ne_md5 *c, const void *data, size_t n);
void ne_md5_final(ne_md5 *c, uint8_t out[16]);
void ne_md5_buf(const void *data, size_t n, uint8_t out[16]);
#endif
