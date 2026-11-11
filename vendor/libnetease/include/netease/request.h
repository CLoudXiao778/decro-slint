#ifndef NE_REQUEST_H
#define NE_REQUEST_H
#include <stddef.h>
#include "netease/jmap.h"

/* Request kernel -- mirrors util.CallWeapi / util.CreateRequest (v1.6.0).
 * One global jar per process, persisted by the CLI on exit.
 *
 * THREAD CONTRACT: the global cookie jar is guarded by an internal mutex
 * (see request.c g_jar_lock). Reformulating a cookie header from the jar, a
 * write back of the live jar, or a fresh request snapshot each take the lock,
 * and HTTP I/O never holds it — so concurrent request-kernel calls from
 * multiple threads (e.g. Android via JNI or a GUI host) are safe and run in
 * parallel. The one remaining caveat is ne_global_jar(), which returns the
 * LIVE handle for the CLI's end-of-process persist; single-threaded hosts
 * (the CLI) may use it, but a multi-threaded host must not treat that handle
 * as safe after the call returns. Matches netease/request.h (v1.6.0). */

typedef struct {
    double code;     /* API business code ("code" field), 200 fallback,
                        520 on transport error (Go convention) */
    long   http_status; /* raw HTTP status from the transport, 0 on
                           transport error (added by the risk-control
                           hardening; existing callers ignore it) */
    char  *body;     /* malloc'd response body */
    size_t body_len;
    int    err;      /* 0 ok; 1 transport error; 2 no code field */
} ne_resp;

void ne_resp_free(ne_resp *r);

/* global jar */
void ne_set_cookie_file(const char *path);
const char *ne_cookie_file(void);
/* (re)create the global jar from the cookie file; missing file = empty jar */
void ne_jar_reload(void);
/* opaque global jar handle (for ne_jar_save_file on exit) */
struct ne_jar;
struct ne_jar *ne_global_jar(void);
/* thread-safe merge of an exported cookie string into the global jar + persist
 * (Android importCookies); unlike ne_global_jar, safe alongside in-flight
 * requests on a multi-threaded host. */
void ne_jar_import_cookies(const char *cookie_str);

/* API base URL. Default "https://music.163.com". Priority: explicit
 * ne_set_api_base() (highest) -> NE_API_BASE env (CLI/test hook) -> default.
 * Embedded hosts (Android, GUI) that cannot rely on the environment should
 * call ne_set_api_base() at startup. */
const char *ne_api_base(void);
void ne_set_api_base(const char *base);

/* util.CallWeapi -- POST <api> with ApiParamsEncode(data).
 * UA = pc, Referer set, cookies from global jar. */
ne_resp *ne_call_weapi(const char *api, const jmap *data);

/* 同 ne_call_weapi 的语义（url 原样、body 严格校验、err 口径一致），但 cookie 头走
 * post_common 的注入版（`__remember_me` + `os` + `appver`，有 MUSIC_U 时补随机
 * `_ntes_nuid`）而不是「活 jar 原样」。
 *
 * 用途：**按权益发 URL** 的端点（`/weapi/song/enhance/player/url/v1`）对未购 VIP 歌
 * 要求请求同时带非空的 `os` 与 `appver`，缺任一个回 `code=-110 / url=null`；而安卓侧
 * 没有代码会把这两个字段写进活 jar（见 request.c 里本函数的文档）。
 *
 * ⚠️ 用 `COOKIES_OS_IOS`（→ os=ios + appver=9.0.65），**不要用 `COOKIES_OS_PC`**
 * —— 后者会让 appver 变成空串，实测同样拿不到 url。 */
ne_resp *ne_call_weapi_cookies(const char *api, const jmap *data,
                               const char *const *extra_cookies);

/* util.CreateRequest("POST", url, data, {Crypto:"weapi", Cookies: extra})
 * — the legacy path used by most read services and the `like` command.
 * Applies the /\w*api/ → /weapi/ URL rewrite, injects the request.go cookie
 * set (os/appver/__remember_me, _ntes_nuid, NMTID on login URLs) and
 * csrf_token before encryption. extra_cookies = {name,value} NULL-terminated
 * pairs. NOTE: `data` is modified (csrf_token added). */
ne_resp *ne_create_weapi(const char *url, jmap *data,
                         const char *const *extra_cookies);
/* CallWeapi-equivalent transport for login flows: no anti-fraud cookie
 * injection (no os/appver/NMTID), clean web request like request.go's
 * NewRequest path. */
ne_resp *ne_create_weapi_clean(const char *url, jmap *data);
/* Same as ne_create_weapi but WITHOUT the /\w*api/ → /weapi/ rewrite: the url
 * is sent verbatim, i.e. it stays on the older /api/ gateway. For the rare
 * endpoint whose /weapi/ route cannot do the job — currently only block-stream
 * pagination: /weapi/homepage/block/page rejects ANY non-empty `cursor` (the
 * "-1" sentinel, 0, or even the cursor it handed out itself) with
 * HTTP 200 + {"code":50002} (74 bytes), while /api/homepage/block/page accepts
 * it. Not used by ne_homepage_block_page today, since the homepage works fine
 * without a cursor on the standard /weapi/ path. */
ne_resp *ne_create_weapi_asis(const char *url, jmap *data,
                              const char *const *extra_cookies);

/* util.CreateRequest(..., {Crypto:"linuxapi"}) — payload
 * {method, url(/api/), params} AES-ECB encrypted, POSTed to
 * /api/linux/forward with a Linux UA. TAKES OWNERSHIP of `data`
 * (embedded as the nested params map — do not free it afterwards). */
ne_resp *ne_call_linuxapi(const char *url, jmap *data,
                          const char *const *extra_cookies);

/* util.CreateRequest(..., {Crypto:"eapi", Url: eapi_path}) — the mobile-API
 * branch (playlist-rename): builds the anti-fraud "header" object (osver/
 * deviceId/appver/versioncode/mobilename/buildver/resolution/__csrf/os/
 * channel/requestId [+MUSIC_U/MUSIC_A]), embeds it as data["header"],
 * AES-ECB-encrypts url+json, rewrites /\w*api/ -> /eapi/ and ALSO sends the
 * header entries as request cookies (request.go does both). The header map
 * is merged INTO `data` (ownership taken by the call). */
ne_resp *ne_call_eapi(const char *url, const char *eapi_path, jmap *data);

/* util.ApplyRequestStrategy — inject os=pc + FIXED fake NMTID into the jar.
 * The fake NMTID is dropped AT THE JAR BOUNDARY (filterJar.SetCookies), so it
 * never reaches memory, disk, or the wire — only os=pc actually takes effect.
 * Kept as an explicit function because the Go call site still exists. */
void ne_apply_request_strategy(void);

/* util.GenerateChainID — "v1_<sDeviceId>_web_login_<ms>"; sDeviceId from
 * jar or freshly generated 52-hex (upper). malloc'd. */
char *ne_generate_chain_id(void);

/* request.go `/\w*api/` rewrite (weapi branch + linuxapi inner url).
 * Replacement is "/weapi/" or "/api/"; a segment not followed by '/' is
 * no match. malloc'd result. Public for the unit tests. */
char *ne_rewrite_api_segment(const char *url, const char *replacement);

/* 风控应对(可选, 默认关闭): 对 -460/高频限流/传输错误做指数退避自动重试,
 * 最多 max_attempts 次; <=0 关闭. 环境变量 NE_RETRY_RISK=<n> 同效.
 * 仅重试"瞬时可恢复"的分类(见 ne_risk_is_transient), -462 行为验证不重试.
 * 注意: 写操作(like/subscribe/track-add 等)若服务端已受理但响应丢失,
 * 重试可能重复生效 —— 写操作场景请保持关闭或自行保证幂等. */
void ne_set_risk_retry(int max_attempts);
#endif
