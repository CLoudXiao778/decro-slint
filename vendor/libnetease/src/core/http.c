/* Transport-agnostic HTTP layer. The default transport is libcurl, compiled
 * in only when NE_HAVE_CURL is set (the desktop builds). On a platform
 * without curl (e.g. Android NDK) no default transport is present; the host
 * app installs one via ne_http_set_transport() — typically an OkHttp-backed
 * transport reached over JNI. Either way the request layer is untouched.
 *
 * Set-Cookie handling: each transport captures Set-Cookie response headers
 * into resp->set_cookies; the request layer reads them straight off the
 * response object, so there is no shared/global cookie back-channel here. */
#include "netease/http.h"
#include "netease/rand.h"
#include "netease/risk.h"
#include "netease/util.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef NE_HAVE_CURL
#include <curl/curl.h>
#endif

/* ── 风控应对配置 (setter 优先, 未设置时回退环境变量) ──
 * 定义在文件前部: curl_request / do_request 都要用, C 要求先声明后使用. */

static char *g_real_ip = NULL;
static int   g_real_ip_set = 0;
static int   g_rate_base = -1;    /* -1 = 未设置 */
static int   g_rate_jitter = -1;
static int   g_no_keepalive = -1;
static int   g_random_cn_ip = -1;

void ne_http_set_real_ip(const char *ip) {
    free(g_real_ip);
    g_real_ip = (ip && *ip) ? ne_xstrdup(ip) : NULL;
    g_real_ip_set = 1;
}
void ne_http_set_rate_limit(int base_ms, int jitter_ms) {
    g_rate_base = base_ms;
    g_rate_jitter = jitter_ms;
}
void ne_http_set_no_keepalive(int on) { g_no_keepalive = on ? 1 : 0; }
void ne_http_set_random_cn_ip(int on) { g_random_cn_ip = on ? 1 : 0; }

static int http_rate_base(void) {
    if (g_rate_base >= 0) return g_rate_base;
    const char *e = getenv("NE_RATE_LIMIT_MS");
    return e ? atoi(e) : 0;
}
static int http_rate_jitter(void) {
    if (g_rate_jitter >= 0) return g_rate_jitter;
    const char *e = getenv("NE_RATE_LIMIT_JITTER_MS");
    return e ? atoi(e) : 0;
}

static void http_pace(void) {
    int base = http_rate_base(), jit = http_rate_jitter();
    if (base <= 0 && jit <= 0) return;
    long d = base;
    if (jit > 0) d += (long)ne_rand_below((uint32_t)(jit + 1));
    if (d > 0) ne_sleep_ms(d);
}

static const char *http_real_ip(void) {
    if (g_real_ip_set) return g_real_ip;
    return getenv("NE_REAL_IP");
}

static int http_random_cn_ip_enabled(void) {
    if (g_random_cn_ip >= 0) return g_random_cn_ip;
    const char *e = getenv("NE_RANDOM_CN_IP");
    return e && *e && *e != '0';
}

const char *ne_http_get_real_ip(void) {
    const char *ip = http_real_ip();
    if (ip && *ip) return ip;
    if (http_random_cn_ip_enabled()) {
        static char auto_ip[16];
        ne_random_cn_ip(auto_ip);
        return auto_ip;
    }
    return NULL;
}

/* ── default libcurl transport ────────────────────────── */
#ifdef NE_HAVE_CURL
/* curl-only toggles: unused in no-curl builds (Android/JNI), so they live
 * inside this block to keep -Wunused-function clean. */
static int http_no_keepalive(void) {
    if (g_no_keepalive >= 0) return g_no_keepalive;
    const char *e = getenv("NE_NO_KEEPALIVE");
    return e && *e && *e != '0';
}
static int http_use_http2(void) {
    const char *e = getenv("NE_HTTP2");
    return e && *e && *e != '0';
}
static int http_browser_headers(void) {
    const char *e = getenv("NE_BROWSER_HEADERS");
    return e && *e && *e != '0';
}

struct body_buf { char *p; size_t len, cap; };

static void body_append(struct body_buf *b, const char *data, size_t n) {
    while (b->len + n + 1 > b->cap) {
        b->cap = b->cap ? b->cap * 2 : 4096;
        b->p = ne_xrealloc(b->p, b->cap);
    }
    memcpy(b->p + b->len, data, n);
    b->len += n;
    b->p[b->len] = '\0';
}

static size_t write_cb(char *ptr, size_t size, size_t nmemb, void *ud) {
    body_append((struct body_buf *)ud, ptr, size * nmemb);
    return size * nmemb;
}

/* Collect Set-Cookie headers into concatenated "name=value; ...\n" lines so
 * the transport can hand them back via resp->set_cookies. */
static size_t header_cb(char *ptr, size_t size, size_t nmemb, void *ud) {
    size_t n = size * nmemb;
    struct body_buf *setcookies = (struct body_buf *)ud;
    if (n > 12 && strncasecmp(ptr, "Set-Cookie:", 11) == 0) {
        const char *v = ptr + 11;
        size_t vn = n - 11;
        while (vn > 0 && (*v == ' ' || *v == '\t')) { v++; vn--; }
        /* strip trailing \r\n */
        while (vn > 0 && (v[vn - 1] == '\n' || v[vn - 1] == '\r')) vn--;
        body_append(setcookies, v, vn);
        body_append(setcookies, "\n", 1);
    }
    return n;
}

static ne_http_resp *curl_request(const char *url, const char *method,
                                  const char *body, const char *content_type,
                                  const char *cookie_header,
                                  const char *user_agent) {
    CURL *curl = curl_easy_init();
    if (!curl) return NULL;

    struct body_buf body_b = {0}, setcookies = {0};
    ne_http_resp *r = ne_xmalloc(sizeof(ne_http_resp));
    memset(r, 0, sizeof *r);

    curl_easy_setopt(curl, CURLOPT_URL, url);
    if (getenv("NE_DEBUG_HTTP")) curl_easy_setopt(curl, CURLOPT_VERBOSE, 1L);
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, write_cb);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &body_b);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, header_cb);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &setcookies);
    curl_easy_setopt(curl, CURLOPT_ACCEPT_ENCODING, "");   /* zlib/gzip auto */
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(curl, CURLOPT_CONNECTTIMEOUT, 10L);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);
    if (http_no_keepalive()) {
        /* 代理换 IP 时禁用连接复用, 让新出口立即生效 */
        curl_easy_setopt(curl, CURLOPT_FORBID_REUSE, 1L);
        curl_easy_setopt(curl, CURLOPT_FRESH_CONNECT, 1L);
    }
    /* NE_HTTP2=1: 显式协商 HTTP/2 (浏览器标配; 标准 curl 默认可能回落 1.1).
     * 需要 libcurl 编译时带 nghttp2, 否则自动回落 HTTP/1.1 无副作用.
     * 注意: 本地 stub (tests/dualrun.py, Python http.server) 仅支持 HTTP/1.1,
     * 测试时不要开启本选项. */
    if (http_use_http2()) {
#ifdef CURL_HTTP_VERSION_2TLS
        curl_easy_setopt(curl, CURLOPT_HTTP_VERSION, CURL_HTTP_VERSION_2TLS);
#endif
    }

    struct curl_slist *hdrs = NULL;
    if (user_agent) {
        char ua_hdr[512];
        snprintf(ua_hdr, sizeof ua_hdr, "User-Agent: %s", user_agent);
        hdrs = curl_slist_append(hdrs, ua_hdr);
    }
    /* NE_BROWSER_HEADERS=1: 补上浏览器标准请求头 (Accept / Accept-Language /
     * Sec-Fetch 系 / Upgrade-Insecure-Requests), 让请求头形态更接近真实
     * 浏览器, 降低 HTTP 层指纹差异. 全部 opt-in, 默认零行为变更. */
    if (http_browser_headers()) {
        hdrs = curl_slist_append(hdrs,
            "Accept: application/json, text/plain, */*");
        hdrs = curl_slist_append(hdrs,
            "Accept-Language: zh-CN,zh;q=0.9,en;q=0.8");
        hdrs = curl_slist_append(hdrs, "Sec-Fetch-Dest: empty");
        hdrs = curl_slist_append(hdrs, "Sec-Fetch-Mode: cors");
        hdrs = curl_slist_append(hdrs, "Sec-Fetch-Site: same-origin");
        hdrs = curl_slist_append(hdrs, "Upgrade-Insecure-Requests: 1");
    }
    /* X-Real-IP / X-Forwarded-For 是国内 IP 时, 可规避海外/数据中心出口
     * 的 460/空 body 风控 —— Binaryify 文档与 api-enhanced / Meting-API
     * 都同时注入这两个头. 来源优先级: 显式 NE_REAL_IP(或 setter) →
     * NE_RANDOM_CN_IP=1 每次自动生成国内 IP. */
    const char *real_ip = http_real_ip();
    char auto_cnip[16] = "";
    if ((!real_ip || !*real_ip) && http_random_cn_ip_enabled()) {
        ne_random_cn_ip(auto_cnip);
        real_ip = auto_cnip;
    }
    if (real_ip && *real_ip) {
        char rip_hdr[512], xff_hdr[640];
        snprintf(rip_hdr, sizeof rip_hdr, "X-Real-IP: %s", real_ip);
        hdrs = curl_slist_append(hdrs, rip_hdr);
        snprintf(xff_hdr, sizeof xff_hdr, "X-Forwarded-For: %s", real_ip);
        hdrs = curl_slist_append(hdrs, xff_hdr);
    }
    if (body && strcmp(method, "POST") == 0) {
        char ct_hdr[256];
        snprintf(ct_hdr, sizeof ct_hdr, "Content-Type: %s",
                 content_type ? content_type : "application/x-www-form-urlencoded");
        hdrs = curl_slist_append(hdrs, ct_hdr);
        curl_easy_setopt(curl, CURLOPT_POSTFIELDS, body);
    }
    /* Go requests.NewRequest sets Referer unconditionally (CallWeapi path);
     * CreateRequest sets it for music.163.com — netease URLs always are,
     * so unconditional matches both. */
    hdrs = curl_slist_append(hdrs, "Referer: https://music.163.com");
    if (cookie_header && *cookie_header) {
        char ck_hdr[8192];
        snprintf(ck_hdr, sizeof ck_hdr, "Cookie: %s", cookie_header);
        hdrs = curl_slist_append(hdrs, ck_hdr);
    }
    if (hdrs) curl_easy_setopt(curl, CURLOPT_HTTPHEADER, hdrs);

    CURLcode rc = curl_easy_perform(curl);
    if (rc != CURLE_OK) {
        r->err = ne_xstrdup(curl_easy_strerror(rc));
        r->status = 0;
    } else {
        long code = 0;
        curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &code);
        r->status = code;
    }
    r->body = body_b.p ? body_b.p : ne_xstrdup("");
    r->body_len = body_b.len;
    r->set_cookies = setcookies.p ? setcookies.p : NULL;

    curl_slist_free_all(hdrs);
    curl_easy_cleanup(curl);
    return r;
}

static const ne_http_transport g_curl_transport = { curl_request };
#endif /* NE_HAVE_CURL */

/* ── installed-transport dispatch + thread-local set-cookie sink ── */

static const ne_http_transport *g_transport = NULL;   /* NULL → default */

static const ne_http_transport *g_default_transport(void) {
#ifdef NE_HAVE_CURL
    return &g_curl_transport;
#else
    return NULL;
#endif
}

void ne_http_set_transport(const ne_http_transport *t) { g_transport = t; }
const ne_http_transport *ne_http_get_transport(void) {
    return g_transport ? g_transport : g_default_transport();
}

void ne_http_resp_free(ne_http_resp *r) {
    if (!r) return;
    free(r->body);
    free(r->err);
    free(r->set_cookies);
    free(r);
}

static ne_http_resp *do_request(const char *url, const char *method,
                                const char *body, const char *content_type,
                                const char *cookie_header,
                                const char *user_agent) {
    const ne_http_transport *t = g_transport ? g_transport : g_default_transport();
    if (!t || !t->request) {
        /* no transport (built without curl and none injected) — fail cleanly
         * instead of crashing */
        ne_http_resp *r = ne_xmalloc(sizeof(ne_http_resp));
        memset(r, 0, sizeof *r);
        r->status = 0;
        r->body = ne_xstrdup("");
        r->err = ne_xstrdup("no http transport installed");
        return r;
    }
    http_pace();   /* 可选: 请求间隔模拟真人节奏 */
    return t->request(url, method, body, content_type, cookie_header, user_agent);
}

ne_http_resp *ne_http_post(const char *url, const char *form_body,
                           const char *cookie_header, const char *user_agent,
                           const char *content_type) {
    return do_request(url, "POST", form_body, content_type,
                      cookie_header, user_agent);
}

ne_http_resp *ne_http_get(const char *url, const char *cookie_header,
                          const char *user_agent) {
    return do_request(url, "GET", NULL, NULL, cookie_header, user_agent);
}

/* ── urlencoding (transport-independent; was curl_easy_escape) ── */

static int url_unreserved(unsigned char ch) {
    return (ch >= 'A' && ch <= 'Z') || (ch >= 'a' && ch <= 'z') ||
           (ch >= '0' && ch <= '9') || ch == '-' || ch == '_' ||
           ch == '.' || ch == '~';
}

static void url_append_escaped(char *dst, size_t *len,
                               const unsigned char *s) {
    static const char hex[] = "0123456789ABCDEF";
    for (; *s; s++) {
        if (url_unreserved(*s)) {
            dst[(*len)++] = (char)*s;
        } else {
            dst[(*len)++] = '%';
            dst[(*len)++] = hex[*s >> 4];
            dst[(*len)++] = hex[*s & 0xF];
        }
    }
}

char *ne_http_form_encode(const char *const *kv, size_t n_pairs) {
    size_t cap = 256, len = 0;
    char *out = ne_xmalloc(cap);
    out[0] = '\0';
    for (size_t i = 0; i < n_pairs; i++) {
        const char *k = kv[2 * i], *v = kv[2 * i + 1];
        /* worst case: 3x each byte of key/value, '=', and '&' or '\0' */
        size_t need = (strlen(k) + strlen(v)) * 3 + 2 + (len ? 1 : 0);
        while (len + need + 1 > cap) { cap *= 2; out = ne_xrealloc(out, cap); }
        if (len) out[len++] = '&';
        url_append_escaped(out, &len, (const unsigned char *)k);
        out[len++] = '=';
        url_append_escaped(out, &len, (const unsigned char *)v);
        out[len] = '\0';
    }
    return out;
}