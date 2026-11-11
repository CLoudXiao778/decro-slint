#ifndef NE_QRENC_H
#define NE_QRENC_H
#include <stddef.h>
#include <stdint.h>

/* Port of github.com/skip2/go-qrcode (as used by netease-cli v1.6.0):
 * qrcode.New(content, qrcode.Medium) + ToSmallString(false) + PNG(480).
 *
 * The module bitmap INCLUDES the 4-module quiet zone, mirroring the Go
 * Bitmap() output (size = modules + 8).
 *
 * bits[y*size + x] != 0  <=>  module is dark (black). */
typedef struct {
    int size;              /* total width incl. quiet zone */
    int version;           /* QR version 1..40 */
    int mask;              /* chosen mask pattern 0..7 */
    unsigned char *bits;   /* size*size, row-major */
} ne_qr;

/* Both return NULL on failure ("no data to encode" / "content too long").
 * err_out (optional) receives the Go error message text. */
ne_qr *ne_qr_new(const char *content, const char **err_out);
void ne_qr_free(ne_qr *q);

/* ToSmallString(false): half-block UTF-8 art with trailing newline per row. */
char *ne_qr_small_string(const ne_qr *q);

/* PNG(size) equivalent: palette (white,black), Paletted colour-type 3,
 * adaptive row filters, zlib level 9 (stored-deflate fallback if libz is
 * unavailable at build time). Returns malloc'd buffer, len via *out_len. */
unsigned char *ne_qr_png(const ne_qr *q, int size, size_t *out_len);
#endif
