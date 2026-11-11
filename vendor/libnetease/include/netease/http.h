#ifndef NE_HTTP_H
#define NE_HTTP_H
#include <stddef.h>

/* Transport-agnostic HTTP wrapper. The default transport is libcurl,
 * installed by default. A replacement transport (e.g. OkHttp-backed on
 * Android) can be installed at runtime with ne_http_set_transport() so the
 * library itself never hard-depends on a given network/TLS stack. Every
 * transport returns the same ne_http_resp shape; gzip/zlib decoding and
 * redirects are the transport's responsibility (curl does them via
 * CURLOPT_ACCEPT_ENCODING / FOLLOWLOCATION), mirroring request.go. */

typedef struct {
    long   status;      /* HTTP status, or 0 on transport error */
    char  *body;        /* malloc'd, NUL-terminated */
    size_t body_len;
    char  *err;         /* malloc'd error string, NULL if none */
    char  *set_cookies; /* owned "name=value\n" lines from Set-Cookie, or NULL */
} ne_http_resp;

void ne_http_resp_free(ne_http_resp *r);

/* A transport implements a single "do a request, get a response" primitive.
 * method is "POST" or "GET" (body/content_type only used for POST);
 * cookie_header may be NULL. Always returns a malloc'd resp, never NULL.
 * Set-Cookie response headers MUST be captured by the transport into
 * resp->set_cookies as the RAW header values, one per '\n'-terminated line
 * (attributes included — the jar filters them; see cookiejar.c). The request
 * layer consumes them directly from the response object (no thread-local
 * back-channel). Both bundled transports do exactly this: curl via
 * CURLOPT_HEADERFUNCTION, Android via OkHttp Response.headers("Set-Cookie"). */
typedef ne_http_resp *(*ne_transport_req_fn)(
    const char *url, const char *method,
    const char *body, const char *content_type,
    const char *cookie_header, const char *user_agent);

typedef struct {
    ne_transport_req_fn request;
} ne_http_transport;

/* Install a custom transport; NULL restores the default (curl). Must be
 * called before any request is made. */
void ne_http_set_transport(const ne_http_transport *t);

const ne_http_transport *ne_http_get_transport(void);

/* POST form: body is "k=v&k2=v2" ALREADY url-encoded by the caller
 * (netease params are base64/hex — we still encode defensively in
 * ne_http_form_encode()). cookie_header may be NULL. */
ne_http_resp *ne_http_post(const char *url,
                           const char *form_body,
                           const char *cookie_header,
                           const char *user_agent,
                           const char *content_type);

ne_http_resp *ne_http_get(const char *url,
                          const char *cookie_header,
                          const char *user_agent);

/* urlencoded "a=1&b=2" builder over (key,value) pairs (transport-independent) */
char *ne_http_form_encode(const char *const *kv, size_t n_pairs);

/* ── 风控应对开关 (全部可选, 默认关闭, 不影响既有行为) ──────────────
 * 详见 docs/RISKS.md. 每个都有等价环境变量, 便于 CLI/脚本零改动启用:
 *
 * - ne_http_set_real_ip(ip):     给请求附加 X-Real-IP 与 X-Forwarded-For 头
 *   (国内 IP 可规避海外/数据中心出口的 460/空 body 风控)。env: NE_REAL_IP
 *   另设 NE_RANDOM_CN_IP=1 或 ne_http_set_random_cn_ip(1) 可在未显式设置时,
 *   每次请求自动生成国内 IP。
 * - ne_http_set_rate_limit(base, jitter): 每次请求前强制间隔
 *   base + rand[0, jitter) 毫秒, 模拟真人节奏。env: NE_RATE_LIMIT_MS
 *   + NE_RATE_LIMIT_JITTER_MS
 * - ne_http_set_no_keepalive(on): 关闭连接复用(HTTP/1.1 keep-alive),
 *   让代理换 IP 立即生效。env: NE_NO_KEEPALIVE
 * - NE_BROWSER_HEADERS=1: 补上浏览器标准请求头(Accept / Accept-Language /
 *   Sec-Fetch 系 / Upgrade-Insecure-Requests), 降低 HTTP 层指纹差异。
 * - NE_HTTP2=1: 显式协商 HTTP/2(浏览器标配; 需 libcurl 带 nghttp2, 否则
 *   回落 HTTP/1.1 无副作用)。本地 stub 测试仅支持 HTTP/1.1, 测试勿开。
 * 传 NULL/-1 恢复"未设置", 之后回退到环境变量取值。 */
void ne_http_set_real_ip(const char *ip);
void ne_http_set_rate_limit(int base_ms, int jitter_ms);
void ne_http_set_no_keepalive(int on);
void ne_http_set_random_cn_ip(int on);

/* Returns the real IP to inject as X-Real-IP / X-Forwarded-For, or NULL.
 * Respects ne_http_set_real_ip() / NE_REAL_IP, then ne_http_set_random_cn_ip()
 * / NE_RANDOM_CN_IP. Non-curl transports call this to replicate the IP
 * injection that curl_request does internally. Uses a static buffer for
 * auto-generated IPs — safe under the single-threaded request contract. */
const char *ne_http_get_real_ip(void);
#endif