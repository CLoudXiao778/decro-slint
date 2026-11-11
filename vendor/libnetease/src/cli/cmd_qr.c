/* qr-family commands: qr-key / qr-check (login flow). Match main.go's
 * stdout protocol byte-for-byte. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "netease/cookiejar.h"
#include "netease/qr.h"
#include "netease/request.h"
#include "netease/util.h"
#include "cli_shared.h"
#include "cmds.h"

int cmd_qr_key(void) {
    double code;
    char *body; size_t body_len;
    char *unikey = ne_qr_get_key(&code, &body, &body_len);

    /* risk-control (-462) retry with a brand-new jar: stale login-fragment
     * cookies in the old file trigger it (main.go qr-key) */
    if (code != 200 || !*unikey) {
        remove(ne_cookie_file());
        ne_jar_reload();
        free(unikey); free(body);
        unikey = ne_qr_get_key(&code, &body, &body_len);
    }

    if (code != 200 || !*unikey) {
        char msg[512];
        const char *m = strstr(body ? body : "", "\"message\"");
        if (m) {
            char *end = strchr(m + 9, '"');
            if (end) {
                size_t n = (size_t)(end - m) < sizeof msg - 32 ? (size_t)(end - m) : sizeof msg - 32;
                snprintf(msg, sizeof msg, "get qr key failed: code=%.0f, %.*s", code, (int)n, m + 9);
            } else {
                snprintf(msg, sizeof msg, "get qr key failed: code=%.0f", code);
            }
        } else if (body && *body) {
            snprintf(msg, sizeof msg, "get qr key failed: code=%.0f, body=%s", code, body);
        } else {
            snprintf(msg, sizeof msg, "get qr key failed: code=%.0f", code);
        }
        free(unikey); free(body);
        cli_die(msg);
    }

    char *url = ne_qr_build_url(unikey);
    /* hand-assembled JSON — the & in the URL must NOT become \u0026
     * (Go json.Marshal would escape it and break the QR payload) */
    printf("{\"unikey\":\"%s\",\"url\":\"%s\"}\n", unikey, url);
    free(url); free(unikey); free(body);
    return 0;
}

int cmd_qr_check(const char *unikey) {
    double code;
    size_t body_len;
    char *body = ne_qr_check(unikey, &code, &body_len);
    if (!body) cli_die("check failed");

    /* code 803 = confirmed: persist the server-issued cookie string */
    if ((long)code == 803) {
        /* "cookie":"..." or data.cookie in the body — extract raw values */
        const char *ck = strstr(body, "\"cookie\"");
        if (ck) {
            ck = strchr(ck + 8, ':');
            if (ck) {
                ck++;
                while (*ck == ' ' || *ck == '\t') ck++;
                if (*ck == '"') {
                    ck++;
                    char *end = strchr(ck, '"');
                    if (end) {
                        char *tmp = ne_xmalloc((size_t)(end - ck) + 1);
                        memcpy(tmp, ck, (size_t)(end - ck));
                        tmp[end - ck] = '\0';
                        ne_jar_merge_cookie_str(ne_global_jar(), tmp);
                        free(tmp);
                    }
                }
            }
        }
    }
    printf("{\"code\":%.0f,\"body\":%s}\n", code, body);
    free(body);
    return 0;
}