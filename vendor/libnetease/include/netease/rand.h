#ifndef NE_RAND_H
#define NE_RAND_H
#include <stddef.h>
#include <stdint.h>

/* OS-entropy random bytes: /dev/urandom on POSIX, rand_s() on Windows. */
int ne_rand_bytes(uint8_t *buf, size_t n);

/* uniform random in [0, n) (rejection sampling over ne_rand_bytes) */
uint32_t ne_rand_below(uint32_t n);

/* 16 chars drawn from [A-Za-z0-9] — mirror of util.NewLen16Rand.
 * NOTE (from cryto.go): the "reverse" key is an INDEPENDENT random string,
 * not the reverse of the first one — the Go loop draws two fresh values per
 * iteration. We keep that behaviour. */
void ne_rand_secret16(char out[17]);      /* NUL-terminated, 16 chars */
#endif
