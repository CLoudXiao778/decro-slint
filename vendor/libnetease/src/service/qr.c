#include "netease/qr.h"
#include "netease/request.h"
#include "netease/util.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* extract "unikey" string field from a flat JSON body */
static char *extract_unikey(const char *body) {
    const char *p = strstr(body, "\"unikey\"");
    if (!p) return ne_xstrdup("");
    p = strchr(p + 7, ':');
    if (!p) return ne_xstrdup("");
    p++;
    while (*p == ' ' || *p == '\t') p++;
    if (*p != '"') return ne_xstrdup("");
    p++;
    size_t cap = 64, len = 0;
    char *out = ne_xmalloc(cap);
    while (*p && *p != '"') {
        if (*p == '\\' && p[1]) p++;   /* raw token, no escapes expected */
        if (len + 2 > cap) { cap *= 2; out = ne_xrealloc(out, cap); }
        out[len++] = *p++;
    }
    out[len] = '\0';
    return out;
}

char *ne_qr_get_key(double *code_out, char **body_out, size_t *body_len) {
    jmap *data = jmap_new();
    jmap_put_int(data, "type", 1);
    jmap_put_bool(data, "noCheckToken", 1);

    char url[640];
    snprintf(url, sizeof url, "%s/weapi/login/qrcode/unikey",
             ne_api_base());
    /* CallWeapi path (request.go): clean web request — no os/appver/NMTID
       injection, matching the working Go login flow. */
    ne_resp *r = ne_create_weapi_clean(url, data);
    jmap_free(data);

    if (code_out) *code_out = r->code;
    if (body_out) *body_out = ne_xstrdup(r->body);
    if (body_len) *body_len = r->body_len;

    char *unikey = ne_xstrdup("");
    if (r->code == 200 && r->body_len > 0 && !r->err) {
        free(unikey);
        unikey = extract_unikey(r->body);
    }
    ne_resp_free(r);
    return unikey;
}

char *ne_qr_check(const char *unikey, double *code_out, size_t *body_len) {
    jmap *data = jmap_new();
    jmap_put_int(data, "type", 1);
    jmap_put_bool(data, "noCheckToken", 1);
    jmap_put(data, "key", unikey);

    /* CheckQR calls ApplyRequestStrategy BEFORE the request (fake NMTID on
     * the wire, kept off disk by the jar filter) */
    ne_apply_request_strategy();

    char url[640];
    snprintf(url, sizeof url, "%s/weapi/login/qrcode/client/login",
             ne_api_base());
    ne_resp *r = ne_create_weapi_clean(url, data);
    jmap_free(data);

    if (code_out) *code_out = r->code;
    char *body = ne_xstrdup(r->body);
    if (body_len) *body_len = r->body_len;
    ne_resp_free(r);
    return body;
}

char *ne_qr_build_url(const char *unikey) {
    char *chain = ne_generate_chain_id();
    char *out = ne_xmalloc(strlen(unikey) + strlen(chain) + 64);
    snprintf(out, strlen(unikey) + strlen(chain) + 64,
             "http://music.163.com/login?codekey=%s&chainId=%s", unikey, chain);
    free(chain);
    return out;
}
