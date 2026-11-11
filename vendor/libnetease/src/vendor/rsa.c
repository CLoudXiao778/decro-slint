/* netease weapi RSA: textbook (no padding) with the public key from
 * cryto.go v1.6.0. The modulus is parsed from the embedded PEM at runtime —
 * a ~40-line DER walk instead of a hand-copied hex constant. */
#include "netease/rsa.h"
#include "netease/encoding.h"
#include "netease/util.h"
#include <string.h>

static const char NETEASE_PEM[] =
    "-----BEGIN PUBLIC KEY-----\n"
    "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDgtQn2JZ34ZC28NWYpAUd98iZ3"
    "7BUrX/aKzmFbt7clFSs6sXqHauqKWqdtLkF2KexO40H1YTX8z2lSgBBOAxLsvakl"
    "V8k4cBFK9snQXE9/DDaFt6Rr7iVZMldczhC0JNgTz+SHXT6CBHuX3e9SdB1Ua44o"
    "ncaTWz7OBGLbCiK45wIDAQAB\n"
    "-----END PUBLIC KEY-----";

/* tiny DER reader: returns payload pointer + length for the tag at *pos */
static const uint8_t *der_next(const uint8_t *p, const uint8_t *end,
                               uint8_t want_tag, size_t *len) {
    if (p + 2 > end || p[0] != want_tag) return NULL;
    p++;
    size_t n = *p++;
    if (n & 0x80) {
        int cnt = (int)(n & 0x7f);
        if (cnt > 4 || p + cnt > end) return NULL;
        n = 0;
        for (int i = 0; i < cnt; i++) n = (n << 8) | *p++;
    }
    if (p + n > end) return NULL;
    *len = n;
    return p;
}

int ne_rsa_load_key(bn *n, uint32_t *e) {
    /* strip PEM armor */
    const char *b64 = NETEASE_PEM;
    while (*b64 && *b64 != '\n') b64++;
    b64++;                                   /* skip BEGIN line */
    const char *end = strstr(b64, "-----END");
    if (!end) return -1;

    /* decode body (concatenated lines) */
    size_t body_max = 1024;
    uint8_t *body = ne_xmalloc(body_max);
    size_t bl = 0;
    for (const char *c = b64; c < end; c++) {
        if (*c == '\n' || *c == '\r' || *c == ' ') continue;
        body[bl++] = (uint8_t)*c;
        if (bl >= body_max) { free(body); return -1; }
    }
    body[bl] = 0;
    size_t der_len = 0;
    uint8_t *der = ne_base64_decode((const char *)body, &der_len);
    free(body);
    if (!der) return -1;

    const uint8_t *p = der, *de = der + der_len;
    size_t len;

    /* SubjectPublicKeyInfo ::= SEQ { SEQ{...}, BITSTRING } */
    p = der_next(p, de, 0x30, &len);
    const uint8_t *seq_end = p ? p + len : NULL;
    if (!p) { free(der); return -1; }
    p = der_next(p, de, 0x30, &len);         /* AlgorithmIdentifier — skip */
    if (!p) { free(der); return -1; }
    p += len;
    p = der_next(p, seq_end, 0x03, &len);    /* BITSTRING */
    if (!p || len == 0) { free(der); return -1; }
    p++; len--;                              /* leading 0 = unused bits */

    /* RSAPublicKey ::= SEQ { INTEGER n, INTEGER e } */
    const uint8_t *rk_end = p + len;
    p = der_next(p, rk_end, 0x30, &len);
    if (!p) { free(der); return -1; }
    rk_end = p + len;
    p = der_next(p, rk_end, 0x02, &len);     /* modulus INTEGER */
    if (!p) { free(der); return -1; }
    while (len > 0 && *p == 0) { p++; len--; }   /* drop sign byte / zeros */
    int rc = bn_from_be(n, p, len);
    const uint8_t *q = p + len;
    size_t elen;
    q = der_next(q, rk_end, 0x02, &elen);
    if (!q) { free(der); return -1; }
    *e = 0;
    for (size_t i = 0; i < elen; i++) *e = (*e << 8) | q[i];
    free(der);
    return rc;
}

size_t ne_rsa_encrypt_secretkey(const uint8_t secret_key[16],
                                const bn *n, uint32_t e,
                                uint8_t *out) {
    /* left-pad with 112 zero bytes to 128 total (cryto.go rsaEncrypt) */
    uint8_t buf[128];
    memset(buf, 0, sizeof(buf));
    memcpy(buf + 112, secret_key, 16);

    bn base;
    if (bn_from_be(&base, buf, sizeof(buf)) != 0) return 0;

    /* exponent 65537 = binary 1 0000 0000 0000 0001:
     * 16 squarings then one final multiply by the base. */
    if (e != 65537) return 0;    /* only the netease key shape is supported */
    bn r = base;
    for (int i = 0; i < 16; i++) {
        bn t;
        bn_mulmod(&t, &r, &r, n);
        r = t;
    }
    bn t;
    bn_mulmod(&t, &r, &base, n);
    r = t;

    return bn_to_be_stripped(&r, out);
}
