/* crypto test-suite: C implementation vs Python mirror of cryto.go */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "netease/aes.h"
#include "netease/crypto.h"
#include "netease/encoding.h"
#include "netease/jmap.h"
#include "netease/md5.h"
#include "netease/rsa.h"

#include "expected.h"

static int failures = 0;

static void check_str(const char *name, const char *got, const char *want) {
    if (strcmp(got, want) == 0) {
        printf("PASS %s\n", name);
    } else {
        failures++;
        printf("FAIL %s\n  got:  %.80s\n  want: %.80s\n", name, got, want);
    }
}

int main(void) {
    /* md5 */
    uint8_t dg[16];
    char *h;
    ne_md5_buf("", 0, dg);
    h = ne_hex_lower(dg, 16); check_str("md5(empty)", h, EXP_MD5_EMPTY); free(h);
    ne_md5_buf("abc", 3, dg);
    h = ne_hex_lower(dg, 16); check_str("md5(abc)", h, EXP_MD5_ABC); free(h);

    /* AES-128 ECB FIPS-197 C.1 vector */
    {
        ne_aes128 ctx;
        uint8_t key[16], pt[16], ct[16];
        for (int i = 0; i < 16; i++) { key[i] = (uint8_t)i; pt[i] = (uint8_t)(0x11 * i); }
        /* pt must equal 00112233...ee ff */
        for (int i = 0; i < 16; i++) pt[i] = (uint8_t)(0x11 * i);
        ne_aes128_init(&ctx, key);
        ne_aes128_encrypt_block(&ctx, pt, ct);
        h = ne_hex_lower(ct, 16); check_str("aes-128nist", h, EXP_AES_NIST); free(h);
    }

    /* jmap Go-style marshal */
    {
        jmap *m = jmap_new();
        jmap_put(m, "a", "<&>");
        jmap_put(m, "b", "line\nnext");
        jmap_put(m, "c", "plain");
        char *s = jmap_marshal(m);
        check_str("jmap-go-escape", s, EXP_JMAP1);
        free(s);
        jmap_free(m);
    }

    /* RSA modulus parsed from the embedded PEM matches the Python DER walk */
    {
        bn n; uint32_t e;
        if (ne_rsa_load_key(&n, &e) != 0) { printf("FAIL rsa-load\n"); failures++; }
        else {
            uint8_t be[BN_LIMBS * 4];
            size_t l = bn_to_be_stripped(&n, be);
            h = ne_hex_lower(be, l); check_str("rsa-modulus", h, EXP_RSA_N_HEX); free(h);
            if (e != 65537) { printf("FAIL rsa-e (%u)\n", e); failures++; }
        }
    }

    /* weapi deterministic */
    {
        jmap *m = jmap_new();
        jmap_put(m, "csrf_token", "");
        jmap_put(m, "ids", "[347230,347231]");
        jmap_put(m, "hello", "<&>world");
        ne_weapi_result r;
        if (ne_weapi_det(m, "A1b2C3d4E5f6G7h8", "Z9y8X7w6V5u4T3s2", &r) != 0) {
            printf("FAIL weapi1\n"); failures++;
        } else {
            check_str("weapi1-params", r.params, EXP_WEAPI1_PARAMS);
            check_str("weapi1-enc", r.enc_sec_key, EXP_WEAPI1_ENC);
            ne_weapi_free(&r);
        }
        jmap_free(m);
    }
    {
        jmap *m = jmap_new();
        jmap_put(m, "csrf_token", "");
        ne_weapi_result r;
        if (ne_weapi_det(m, "0123456789abcdef", "fedcba9876543210", &r) != 0) {
            printf("FAIL weapi2\n"); failures++;
        } else {
            check_str("weapi2-params", r.params, EXP_WEAPI2_PARAMS);
            check_str("weapi2-enc", r.enc_sec_key, EXP_WEAPI2_ENC);
            ne_weapi_free(&r);
        }
        jmap_free(m);
    }

    /* eapi with nested header */
    {
        jmap *m = jmap_new();
        jmap_put(m, "ids", "[347230]");
        jmap_put(m, "br", "128000");
        jmap *hdr = jmap_new();
        jmap_put(hdr, "os", "pc");
        jmap_put(hdr, "appver", "9.0.65");
        jmap_put_map(m, "header", hdr);
        char *p = ne_eapi("/api/song/enhance/player/url", m);
        if (!p) { printf("FAIL eapi1\n"); failures++; }
        else { check_str("eapi1-params", p, EXP_EAPI1_PARAMS); free(p); }
        jmap_free(m);
    }

    /* weapi random path smoke test: keys differ each call, encSecKey hex */
    {
        jmap *m = jmap_new();
        jmap_put(m, "csrf_token", "");
        jmap_put(m, "q", "test");
        ne_weapi_result r1, r2;
        ne_weapi(m, &r1);
        ne_weapi(m, &r2);
        int ok = r1.params && r2.params && strcmp(r1.params, r2.params) != 0
              && strlen(r1.enc_sec_key) <= 256 && strlen(r1.enc_sec_key) > 0;
        printf("%s weapi-random\n", ok ? "PASS" : "FAIL");
        if (!ok) failures++;
        ne_weapi_free(&r1);
        ne_weapi_free(&r2);
        jmap_free(m);
    }

    printf("\n%s (%d failures)\n", failures ? "TESTS FAILED" : "ALL TESTS PASSED", failures);
    return failures ? 1 : 0;
}
