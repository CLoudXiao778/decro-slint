/* Keep alloc/free in the same C runtime as libnetease.  The Rust side must
 * never free libnetease's malloc'd strings with a different CRT heap. */
#include <stddef.h>
#include <stdlib.h>

void *decro_alloc(size_t n) { return malloc(n); }
void decro_free(void *p) { free(p); }
