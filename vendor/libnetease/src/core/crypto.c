/* weapi / eapi / linuxapi — line-by-line mirror of cryto.go (v1.6.0). */
#include "netease/crypto.h"
#include "netease/aes.h"
#include "netease/encoding.h"
#include "netease/md5.h"
#include "netease/rand.h"
#include "netease/rsa.h"
#include "netease/util.h"
#include <stdio.h>
#include <string.h>

static const uint8_t IV[16]        = {'0','1','0','2','0','3','0','4','0','5','0','6','0','7','0','8'};
static const uint8_t PRESET_KEY[16]= {'0','C','o','J','U','m','6','Q','y','w','8','W','8','j','u','d'};
static const uint8_t LINUX_KEY[16] = {'r','F','g','B','&','h','#','%','2','?','^','e','D','g',':','Q'};
static const uint8_t EAPI_KEY[16]  = {'e','8','2','c','k','e','n','h','8','d','i','c','h','e','n','8'};

int ne_weapi_det(const jmap *data,
                 const char secret_key[16], const char re_secret_key[16],
                 ne_weapi_result *out) {
    memset(out, 0, sizeof(*out));

    char *text = jmap_marshal(data);
    if (!text) return -1;

    /* params = b64( cbc( b64( cbc(text, presetKey, iv) ), reSecretKey, iv ) ) */
    size_t l1 = 0;
    uint8_t *c1 = ne_aes_cbc_encrypt((const uint8_t *)text, strlen(text),
                                     PRESET_KEY, IV, &l1);
    free(text);
    if (!c1) return -1;
    char *b1 = ne_base64_encode(c1, l1);
    free(c1);
    if (!b1) return -1;

    size_t l2 = 0;
    uint8_t *c2 = ne_aes_cbc_encrypt((const uint8_t *)b1, strlen(b1),
                                     (const uint8_t *)re_secret_key, IV, &l2);
    free(b1);
    if (!c2) return -1;
    out->params = ne_base64_encode(c2, l2);
    free(c2);
    if (!out->params) return -1;

    /* encSecKey = hex( rsa(secretKey) ) */
    bn n; uint32_t e;
    if (ne_rsa_load_key(&n, &e) != 0) { ne_weapi_free(out); return -1; }
    uint8_t rsa_out[128];
    size_t rl = ne_rsa_encrypt_secretkey((const uint8_t *)secret_key, &n, e, rsa_out);
    if (rl == 0) { ne_weapi_free(out); return -1; }
    out->enc_sec_key = ne_hex_lower(rsa_out, rl);
    return out->enc_sec_key ? 0 : -1;
}

int ne_weapi(const jmap *data, ne_weapi_result *out) {
    char sk[17], rsk[17];
    /* cryto.go NewLen16Rand: the second value is the REVERSE of the first
     * (randByteReverse[15-i] uses the SAME rand.Int result), NOT an
     * independent random.  The server derives the layer-2 key by reversing
     * the RSA-delivered secretKey, so rsk MUST be reverse(sk). */
    ne_rand_secret16(sk);
    for (int i = 0; i < 16; i++) rsk[i] = sk[15 - i];
    rsk[16] = '\0';
    return ne_weapi_det(data, sk, rsk, out);
}

void ne_weapi_free(ne_weapi_result *r) {
    if (!r) return;
    free(r->params);
    free(r->enc_sec_key);
    r->params = NULL;
    r->enc_sec_key = NULL;
}

char *ne_eapi_det(const char *url, const jmap *data) {
    char *text = jmap_marshal(data);
    if (!text) return NULL;

    /* message = "nobody" + url + "use" + text + "md5forencrypt" */
    size_t mlen = 6 + strlen(url) + 3 + strlen(text) + 13;
    char *message = ne_xmalloc(mlen + 1);
    snprintf(message, mlen + 1, "nobody%suse%smd5forencrypt", url, text);

    uint8_t dg[16];
    ne_md5_buf(message, strlen(message), dg);
    char *digest = ne_hex_lower(dg, 16);
    free(message);

    /* dd = url + "-36cd479b6b5-" + text + "-36cd479b6b5-" + digest */
    size_t dlen = strlen(url) + 13 + strlen(text) + 13 + strlen(digest);
    char *dd = ne_xmalloc(dlen + 1);
    snprintf(dd, dlen + 1, "%s-36cd479b6b5-%s-36cd479b6b5-%s", url, text, digest);
    free(text);
    free(digest);

    size_t cl = 0;
    uint8_t *c = ne_aes_ecb_encrypt((const uint8_t *)dd, strlen(dd), EAPI_KEY, &cl);
    free(dd);
    if (!c) return NULL;
    char *params = ne_hex_upper(c, cl);
    free(c);
    return params;
}

char *ne_eapi(const char *url, const jmap *data) {
    return ne_eapi_det(url, data);   /* no randomness in eapi */
}

char *ne_linuxapi(const jmap *data) {
    char *text = jmap_marshal(data);
    if (!text) return NULL;
    size_t cl = 0;
    uint8_t *c = ne_aes_ecb_encrypt((const uint8_t *)text, strlen(text),
                                    LINUX_KEY, &cl);
    free(text);
    if (!c) return NULL;
    char *eparams = ne_hex_upper(c, cl);
    free(c);
    return eparams;
}
