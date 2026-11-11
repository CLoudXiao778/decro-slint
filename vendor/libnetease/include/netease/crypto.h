#ifndef NE_CRYPTO_H
#define NE_CRYPTO_H
#include <stddef.h>
#include "jmap.h"

/* ── weapi (cryto.go Weapi) ───────────────────────────────
 * Caller adds csrf_token to `data` BEFORE calling (mirror of request.go).
 * With random keys internally; returns malloc'd {"params","encSecKey"} pair. */
typedef struct {
    char *params;      /* base64 */
    char *enc_sec_key; /* lowercase hex */
} ne_weapi_result;

int ne_weapi(const jmap *data, ne_weapi_result *out);
void ne_weapi_free(ne_weapi_result *r);

/* deterministic variant (fixed keys) — used by the test-suite to compare
 * byte-for-byte against the Go implementation */
int ne_weapi_det(const jmap *data,
                 const char secret_key[16], const char re_secret_key[16],
                 ne_weapi_result *out);

/* ── eapi (cryto.go Eapi) ─────────────────────────────────
 * url here is options.Url (the /api/... path), data already contains the
 * nested "header" object. Returns malloc'd uppercase-hex params. */
char *ne_eapi_det(const char *url, const jmap *data);
char *ne_eapi(const char *url, const jmap *data);   /* identical — no randomness */

/* ── linuxapi (cryto.go Linuxapi) ──────────────────────── */
char *ne_linuxapi(const jmap *data);   /* uppercase hex "eparams" */
#endif
