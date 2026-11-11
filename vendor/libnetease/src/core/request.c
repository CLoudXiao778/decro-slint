/* request.go kernel port (v1.6.0). */
#include "netease/request.h"
#include "netease/cookiejar.h"
#include "netease/crypto.h"
#include "netease/deviceids.h"
#include "netease/encoding.h"
#include "netease/http.h"
#include "netease/inflate.h"
#include "netease/jval.h"
#include "netease/rand.h"
#include "netease/risk.h"
#include "netease/util.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* chooseUserAgent("pc") from request.go — 宏而非指针, 供 UA_PC_POOL
 * 做常量初始化(C11 要求数组初值必须是常量表达式) */
#define UA_PC \
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 " \
    "(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0"

/* UA 轮换池(仅 NE_UA_ROTATE=1 时启用; 默认仍用 UA_PC 保持字节级兼容).
 * 全部为 PC 端 Web UA, 与 os=pc cookie 语义一致. */
static const char *UA_PC_POOL[] = {
    UA_PC,
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 "
    "(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0",
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 "
    "(KHTML, like Gecko) Version/17.4 Safari/605.1.15",
    "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0",
    NULL
};

static const char *choose_ua(void) {
    if (!getenv("NE_UA_ROTATE")) return UA_PC;
    static unsigned seq = 0;
    size_t n = 0;
    while (UA_PC_POOL[n]) n++;
    unsigned pick = (seq++ + (unsigned)ne_rand_below(1000)) % (unsigned)n;
    return UA_PC_POOL[pick];
}

static ne_jar *g_jar = NULL;
static char   *g_cookie_path = NULL;
/* Content fingerprint of the last successful ne_jar_save_file() (guarded by
 * g_jar_lock). Lets the Set-Cookie path skip redundant disk writes. */
static char   *g_cookie_fp = NULL;

/* Guards the process-global jar against concurrent request threads. Any read
 * that feeds values used beyond the lock scope (ne_call_eapi's extras, for
 * example) takes a jar_snapshot() first, so pointers never dangle. First
 * caller initializes the mutex once on the startup thread (see util.h
 * NE_MUTEX_*). */
static ne_mutex g_jar_lock;
static int g_jar_lock_init = 0;
static void jar_lock(void) {
    if (!g_jar_lock_init) { NE_MUTEX_INIT(g_jar_lock); g_jar_lock_init = 1; }
    NE_MUTEX_LOCK(g_jar_lock);
}
static void jar_unlock(void) { NE_MUTEX_UNLOCK(g_jar_lock); }

/* API base override. Priority: explicit ne_set_api_base() (highest), then the
 * NE_API_BASE env fallback (test hook), then the production default. The env
 * path is a convenience for CLI/test hosts; embedded hosts (Android, GUI) that
 * cannot rely on the environment must call ne_set_api_base() up front. */
static char *g_api_base = NULL;

static const char *api_base(void) {
    const char *b = g_api_base;
    if (!b || !*b) b = getenv("NE_API_BASE");
    if (b && *b) {
        static char base[512];
        snprintf(base, sizeof base, "%s", b);
        /* strip trailing slash */
        size_t l = strlen(base);
        while (l > 0 && base[l - 1] == '/') base[--l] = '\0';
        return base;
    }
    return "https://music.163.com";
}

const char *ne_api_base(void) { return api_base(); }

void ne_set_api_base(const char *base) {
    free(g_api_base);
    g_api_base = (base && *base) ? ne_xstrdup(base) : NULL;
}

void ne_set_cookie_file(const char *path) {
    free(g_cookie_path);
    g_cookie_path = ne_xstrdup(path);
}
const char *ne_cookie_file(void) { return g_cookie_path; }

/* Create/load the global jar exactly once. MUST be called with g_jar_lock
 * held (via jar_lock()). */
static void init_jar_locked(void) {
    if (g_jar) return;
    g_jar = ne_jar_new();
    if (g_cookie_path) ne_jar_load_file(g_jar, g_cookie_path);
    /* GetGlobalCookieJar: ensure sDeviceId exists (v1.6.0 behaviour) */
    if (!ne_jar_get(g_jar, "sDeviceId")) {
        static const char hexchars[] = "0123456789ABCDEF";
        char id[53];
        for (int i = 0; i < 52; i++) id[i] = hexchars[ne_rand_below(16)];
        id[52] = '\0';
        ne_jar_set(g_jar, "sDeviceId", id);
    }
}

/* Persist the global jar to g_cookie_path. MUST be called with g_jar_lock held.
 *
 * 2026-10-07: 此前只有 ne_jar_import_cookies() 落盘，服务端在**响应里轮转**的
 * cookie（MUSIC_U 续期、__csrf 重发）只活在内存 ⇒ 冷启动 reload 回来的是旧值，
 * 写操作（点赞 / 收藏）可能拿着过期 csrf 被拒。Android 宿主尤其吃到这个亏：
 * 它只在一处导入 cookie，之后全靠请求响应维护登录态。
 *
 * 只在真的收到 Set-Cookie 时才走到这里，中间还用内容指纹挡一道：没变化就不
 * 写盘（播放场景 GET 频繁，不能每次响应都做 IO）。force=1 用于导入路径——
 * 第一次导入必须落盘，不管指纹看起来是否"没变"。 */
static void jar_persist_locked(int force) {
    if (!g_cookie_path || !*g_cookie_path || !g_jar) return;
    char *fp = ne_jar_cookie_header(g_jar);
    if (!fp) return;
    if (!force && g_cookie_fp && strcmp(fp, g_cookie_fp) == 0) {
        free(fp);
        return;
    }
    if (ne_jar_save_file(g_jar, g_cookie_path) != 0) {
        /* 写失败（磁盘满 / 权限）就不记指纹：下一次带着内容变化的响应还能再写。
         * 若这里照样记指纹，就会永久停在「内存是新的、盘上是旧的」——而这正是
         * 这条路径要修的那个问题本身。 */
        free(fp);
        return;
    }
    free(g_cookie_fp);
    g_cookie_fp = fp;
}

/* Hard reload from disk. The request pipeline uses init-on-first-access
 * (init_jar_locked) rather than constant reloads; this is the CLI persist /
 * refresh path. */
void ne_jar_reload(void) {
    jar_lock();
    if (g_jar) ne_jar_free(g_jar);
    g_jar = NULL;
    free(g_cookie_fp);        /* jar 换了一份，旧指纹不能再认 */
    g_cookie_fp = NULL;
    init_jar_locked();
    jar_unlock();
}

/* Snapshot the global jar's cookies into a fresh scratch jar. The lock is
 * held only for the quick copy; afterwards callers read the scratch (their
 * owned copy) while other threads may write the live jar. This lets the
 * request-assembly read a consistent, non-dangling view AND keeps HTTP I/O
 * (which never touches the jar) outside the lock — so concurrent requests
 * actually run in parallel instead of serializing. */
static ne_jar *jar_snapshot(void) {
    jar_lock();
    if (!g_jar) init_jar_locked();
    ne_jar *snap = ne_jar_new();
    char *hdr = ne_jar_cookie_header(g_jar);
    ne_jar_merge_cookie_str(snap, hdr);
    free(hdr);
    jar_unlock();
    return snap;
}

/* Build a fresh cookie header string from the live jar, under lock. Returns a
 * malloc'd string owned by the caller (already a private copy, safe to use
 * after the lock is released). */
static char *jar_cookie_header_alloc(void) {
    jar_lock();
    if (!g_jar) init_jar_locked();
    char *h = ne_jar_cookie_header(g_jar);
    jar_unlock();
    return h;
}

/* Merge Set-Cookie lines delivered by the transport directly from the
 * response object — no thread-local back-channel (see the thread contract
 * note in request.h).
 *
 * 落盘放在这里（而不是"每次请求结束"）：只有服务端真的轮转了 cookie 才需要
 * 持久化，而带 Set-Cookie 的响应是少数。写盘条件是内容指纹有变化。 */
static void jar_sync_set_cookies(const char *set_cookies) {
    if (!set_cookies || !*set_cookies) return;
    char *copy = ne_xstrdup(set_cookies);
    jar_lock();
    if (!g_jar) init_jar_locked();
    char *save = NULL;
    for (char *line = strtok_r(copy, "\n", &save); line;
         line = strtok_r(NULL, "\n", &save))
        /* 走 Set-Cookie 专用合并（认 Max-Age / Expires 的删除语义）而不是
         * 通用的 cookie 串合并：服务端注销一个 cookie 的标准做法是下发
         * `name=; Expires=Thu, 01 Jan 1970 ...`，不认这条属性的话，被注销的
         * cookie 会以「空值」形态永远留在 jar 与磁盘里，而且照样出现在之后
         * 每个请求的 Cookie 头上。 */
        ne_jar_merge_set_cookie(g_jar, line);
    jar_persist_locked(0);
    jar_unlock();
    free(copy);
}

void ne_resp_free(ne_resp *r) {
    if (!r) return;
    free(r->body);
    free(r);
}

/* parse "code" as a double from a JSON body (jsonparser.GetFloat semantics:
 * missing field => 200). The code field is read from the TOP LEVEL of the
 * object JSON — a naive first-`"code"` scan is wrong for responses that
 * embed a nested `"code":0` (e.g. per-song privilege data in
 * weapi/v1/artist/{id} and weapi/v1/artist/songs) before the real top-level
 * `"code":200`. Parse the tree so only the root's code counts; anything
 * unparseable keeps the 200 fallback. */
static double parse_code(const char *body) {
    if (!body) return 200;
    /* 2026-10 性能修正：原实现每次全树解析（358KB 真实响应 ~11ms，节点
     * 逐 malloc）只为读顶层 code。换成等价的流式扫描（jval.c，零分配），
     * 语义严格对齐：非法/无 code/非数字 → 200 兜底不变。 */
    double v;
    if (ne_jval_scan_top_code(body, &v)) return v;
    return 200;
}

/* 响应体压缩探测解压（见 include/netease/inflate.h）。
 *
 * 为什么在内核里做、而不是交给传输层：传输层是**可注入**的，而各家 HTTP 栈的
 * 自动解压覆盖不齐 —— OkHttp 只解 `Content-Encoding: gzip`（Android 宿主走的
 * 就是这条），curl 只解它自己在 Accept-Encoding 里宣告过的编码，而服务端若
 * **不带** Content-Encoding 直接发 zlib 体，则谁都不解。上游 Go 版正是在响应
 * 处手动补了这一手（request.go「数据被压缩 进行解码」）。放在 finish() 里，
 * 两条传输路径拿到同一份保护，且注入传输层的新宿主也不必自己实现。
 *
 * 判据三重，缺一不可 —— **宁可不解压，也不能把普通数据当压缩体啃掉**：
 *   ① 有公认的包装头（gzip 1f 8b / zlib 78 ..）；raw deflate 无头可辨，不猜；
 *   ② 结构合法、解得出来（解压器内部校验，失败即 NULL）；
 *   ③ 解出来的东西**像 JSON**（首个非空白字符是 '{' 或 '['）。
 * 第 ③ 条是兜底：网易云的响应要么是 JSON，要么是空体/网关错误页，任何误判
 * （图片、HTML）都不会同时满足三条。返回 NULL 时调用方原样使用原始 body。 */
static char *decompress_json_body(const char *body, size_t len, size_t *out_len) {
    if (!body || len < 2) return NULL;
    const uint8_t *in = (const uint8_t *)body;
    if (!ne_inflate_has_gzip_header(in, len) && !ne_inflate_has_zlib_header(in, len))
        return NULL;

    size_t n = 0;
    uint8_t *raw = ne_inflate(in, len, &n, 0);
    if (!raw) return NULL;

    size_t i = 0;
    while (i < n && (raw[i] == ' ' || raw[i] == '\t' || raw[i] == '\r' || raw[i] == '\n')) i++;
    if (i >= n || (raw[i] != '{' && raw[i] != '[')) {
        free(raw);
        return NULL;
    }

    char *s = ne_xmalloc(n + 1);
    memcpy(s, raw, n);
    s[n] = '\0';
    free(raw);
    *out_len = n;
    return s;
}

static ne_resp *finish(ne_http_resp *h) {
    ne_resp *r = ne_xmalloc(sizeof(ne_resp));
    memset(r, 0, sizeof *r);
    r->http_status = h ? h->status : 0;
    if (!h || h->status == 0 || h->err) {
        r->code = 520;
        r->body = ne_xstrdup(h && h->err ? h->err : "transport error");
        r->err = 1;
    } else {
        jar_sync_set_cookies(h ? h->set_cookies : NULL);
        /* 按**长度**复制，不能用 ne_xstrdup —— 那个走 strlen，而压缩响应体
         * 里必然出现 0x00 字节（见下面 decompress_json_body）。用 strlen 会
         * 把 body 截到第一个 NUL，而 body_len 仍是原始长度，随后按 body_len
         * 去读就会越界读，且解压永远失败。 */
        size_t blen = h->body ? h->body_len : 0;
        r->body = ne_xmalloc(blen + 1);
        if (blen) memcpy(r->body, h->body, blen);
        r->body[blen] = '\0';
        r->body_len = blen;
        /* 压缩体先还原再交给 parse_code / 调用方 —— 否则真实响应会被解成
         * 乱码、code 落到 200 兜底，表现为「接口通了但内容全错」这种最难查的
         * 症状（业务错误码 401 也会被读成 200）。 */
        char *plain = decompress_json_body(r->body, r->body_len, &r->body_len);
        if (plain) {
            free(r->body);
            r->body = plain;
        }
        r->code = parse_code(r->body);
        r->err = 0;
    }
    ne_http_resp_free(h);
    return r;
}

/* ── 风控重试(可选, 默认关闭) ─────────────────────────────────────
 * 对 -460/高频/传输错误做指数退避重试, 复用同一份已加密 form —— 与服务
 * 端网关限流场景的社区实践一致(重发相同密文, 偶发限流可自愈). -462 行为
 * 验证不在瞬时可恢复集合内, 不会自动重试. 写操作场景请保持关闭(见
 * request.h 中 ne_set_risk_retry 的说明). */
static int g_risk_retry = -1;   /* -1 = 未设置, 回退 NE_RETRY_RISK */

void ne_set_risk_retry(int max_attempts) { g_risk_retry = max_attempts; }

static int risk_retry_max(void) {
    if (g_risk_retry >= 0) return g_risk_retry;
    const char *e = getenv("NE_RETRY_RISK");
    return e ? atoi(e) : 0;
}

static ne_resp *finish_post_retry(const char *url, const char *form,
                                  const char *cookies, const char *ua) {
    int max_retries = risk_retry_max();
    for (int attempt = 0;; attempt++) {
        ne_http_resp *h = ne_http_post(url, form, cookies, ua, NULL);
        ne_resp *r = finish(h);
        if (attempt >= max_retries) return r;
        ne_risk_class c = ne_risk_classify(r);
        if (ne_risk_is_transient(c)) {
            ne_resp_free(r);
            ne_sleep_ms(ne_risk_backoff_ms(attempt, 8000));
            continue;
        }
        return r;
    }
}

/* CallWeapi 系两条入口共用的「编码失败」回执（request.go 侧是 (520, ...)）。 */
static ne_resp *callweapi_encode_failed(void) {
    ne_resp *r = ne_xmalloc(sizeof(ne_resp));
    r->code = 520; r->body = ne_xstrdup("encode failed");
    r->body_len = strlen(r->body); r->err = 1;
    return r;
}

/* CallWeapi (request.go:375): unlike CreateRequest it VALIDATES the body —
 * must unmarshal into map[string]interface{} with a numeric top-level
 * "code", else (0, body, err). Transport error is also (0, err). The song-url
 * shell keys its fallback off this err.
 *
 * 2026-10 性能修正：原实现把 body 再全树解析一遍（finish 里 parse_code 已经
 * 解析过同一 body——每大响应两次建树纯浪费）。改用流式扫描做 CallWeapi 的
 * 校验：语义不变（数字 code → code；否则 code=0, err=2）。
 *
 * 2026-10-16 起由 ne_call_weapi / ne_call_weapi_cookies 共用：**两条入口的 err
 * 口径必须逐字一致**（音质回退链读的就是它），所以校验只留这一份。 */
static void callweapi_validate_body(ne_resp *r) {
    if (r->err == 1) {
        r->code = 0;
        return;
    }
    double code;
    if (ne_jval_scan_top_code(r->body, &code)) {
        r->code = code;
    } else {
        r->code = 0;
        r->err = 2;
    }
}

ne_resp *ne_call_weapi(const char *api, const jmap *data) {
    ne_weapi_result enc;
    if (ne_weapi(data, &enc) != 0) return callweapi_encode_failed();
    const char *kv[4] = { "params", enc.params, "encSecKey", enc.enc_sec_key };
    char *form = ne_http_form_encode(kv, 2);
    char *cookies = jar_cookie_header_alloc();

    ne_resp *r = finish_post_retry(api, form, cookies, choose_ua());
    free(form); free(cookies);
    ne_weapi_free(&enc);
    callweapi_validate_body(r);
    return r;
}

/* RandStringRunes(16) then hex — request.go's _ntes_nuid / NMTID generator */
static char *rand_hex_of_16(void) {
    static const char letters[] =
        "1234567890abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    char raw[17];
    for (int i = 0; i < 16; i++) raw[i] = letters[ne_rand_below(62)];
    raw[16] = '\0';
    return ne_hex_lower((const uint8_t *)raw, 16);
}

/* request.go regex `/\w*api/` → "/weapi/" (or "/api/" for linuxapi). The
 * Go pattern consumes BOTH slashes (segment plus its trailing '/'), the
 * replacement restores them — so "/api/x" → "/weapi/x", exactly one slash.
 * A segment NOT followed by '/' is no match (regex needs the tail slash);
 * idempotent for URLs already carrying /weapi/. Returns malloc'd URL. */
char *ne_rewrite_api_segment(const char *url, const char *replacement) {
    const char *path = strstr(url, "://");
    path = path ? strchr(path + 3, '/') : strchr(url, '/');
    if (!path) return ne_xstrdup(url);

    /* scan path segments */
    const char *seg = path + 1;
    for (;;) {
        const char *end = seg;
        while (*end && *end != '/' && *end != '?' && *end != '#') end++;
        size_t slen = (size_t)(end - seg);
        if (slen >= 3 && *end == '/') {
            int is_word = 1;
            for (size_t i = 0; i < slen; i++) {
                char c = seg[i];
                if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
                      (c >= '0' && c <= '9') || c == '_')) { is_word = 0; break; }
            }
            /* \w*api: segment ends with "api" */
            if (is_word && slen >= 3 &&
                strncmp(seg + slen - 3, "api", 3) == 0) {
                /* Go: match "/<seg>/" (both slashes) → replace with the
                 * replacement verbatim. Keep everything BEFORE the leading
                 * slash, then splice replacement + rest-after-consumed-slash. */
                size_t cut = (size_t)(seg - url) - 1;         /* excl. leading '/' */
                const char *rest = end + 1;                   /* consumed slash */
                size_t repl_len = strlen(replacement);        /* "/weapi/" */
                char *out = ne_xmalloc(cut + repl_len + strlen(rest) + 1);
                memcpy(out, url, cut);                        /* ...host */
                memcpy(out + cut, replacement, repl_len);     /* /weapi/ */
                strcpy(out + cut + repl_len, rest);
                return out;
            }
        }
        if (!*end) break;
        seg = end + 1;
    }
    return ne_xstrdup(url);
}

/* shared CreateRequest transport for weapi/linuxapi: cookie assembly
 * (jar + extras + __remember_me/os/appver [+ _ntes_nuid] [+ NMTID on login
 * URLs], checked against the ORIGINAL url like request.go).
 * With clean=1 the anti-fraud cookie injection is skipped and the request
 * carries only the jar cookies verbatim — this is the CallWeapi / NewRequest
 * path (request.go:375/298) used by login flows, which must NOT advertise a
 * mobile os/appver alongside a web chainId (that mismatch is what netease
 * risk-control flags on QR login). */
static ne_resp *post_common(const char *orig_url, const char *post_url,
                            const char *form, const char *ua,
                            const char *const *extra_cookies, int clean) {
    /* Scratch jar = locked snapshot of the live jar; everything from here on
     * touches scratch only, so no lock is held across the HTTP call. */
    ne_jar *scratch = jar_snapshot();

    const char *os = "ios";
    const char *appver_extra = NULL;
    for (size_t i = 0; extra_cookies && extra_cookies[2 * i]; i++) {
        if (strcmp(extra_cookies[2 * i], "os") == 0 && extra_cookies[2 * i + 1])
            os = extra_cookies[2 * i + 1];
        if (strcmp(extra_cookies[2 * i], "appver") == 0 && extra_cookies[2 * i + 1])
            appver_extra = extra_cookies[2 * i + 1];
    }
    /* request.go: appver = Ternary(os != "pc", "9.0.65", "") unless a cookie
     * provides one */
    const char *appver = appver_extra ? appver_extra
                      : (strcmp(os, "pc") != 0 ? "9.0.65" : "");

    for (size_t i = 0; extra_cookies && extra_cookies[2 * i]; i++)
        if (extra_cookies[2 * i + 1])
            ne_jar_set(scratch, extra_cookies[2 * i], extra_cookies[2 * i + 1]);
    if (!clean) {
        ne_jar_set(scratch, "__remember_me", "true");
        ne_jar_set(scratch, "os", os);
        ne_jar_set(scratch, "appver", appver);
        if (ne_jar_get(scratch, "MUSIC_U")) {
            char *nuid = rand_hex_of_16();
            ne_jar_set(scratch, "_ntes_nuid", nuid);
            free(nuid);
        }
        if (strstr(orig_url, "login")) {
            char *nmtid = rand_hex_of_16();
            ne_jar_set(scratch, "NMTID", nmtid);   /* request-level random */
            free(nmtid);
        }
    }

    char *cookies = ne_jar_cookie_header(scratch);
    ne_resp *r = finish_post_retry(post_url, form, cookies, ua);
    free(cookies);
    ne_jar_free(scratch);
    return r;
}

/* ne_call_weapi 的**注入版**：CallWeapi 语义逐字不动（url 原样发出、不做
 * /api/ → /weapi/ 重写；body 严格校验，err 口径由 callweapi_validate_body 保证
 * 与 ne_call_weapi 完全一致），只把 cookie 头的组装从「活 jar 原样」换成
 * post_common 的注入版（`__remember_me` + `os` + `appver`，MUSIC_U 在场时补一个
 * 随机 `_ntes_nuid`）。
 *
 * ## 为什么需要它（2026-10-16）
 * `/weapi/song/enhance/player/url/v1` 是**按权益发 URL** 的端点：对「未购 VIP 歌」
 * 它要求请求同时带**非空的 `os` 与 `appver`**，缺任一个一律回
 * `{code:200, data:[{code:-110, url:null, freeTrialInfo:null}]}`（免费与低音质免费歌
 * 不看这两个字段 ⇒ 症状恰好是「只有 VIP 歌播不了」，连 30 秒试听片段都没有）。
 *
 * 而 ne_call_weapi 只发活 jar，**安卓侧活 jar 里永远没有这两个字段**：
 * post_common 写的是 `jar_snapshot()` 出来的 scratch 副本（请求完即 free，不落回活
 * jar），唯一写活 jar 的 ne_apply_request_strategy() 只在 qr.c:63（扫码登录）与
 * ne_login_refresh()（services.c:575）里被调，**JNI 两个都没接**；App 的登录又只有
 * 「WebView 导入 cookie」一条路。于是这个端点成了全仓库唯一依赖「jar 恰好带
 * os/appver」的调用 —— 与它同族的所有读端点（ne_recommend_songs / ne_radio_get /
 * ne_homepage_block_page / ne_playlist_detail …）都走 post_common，从来不受影响。
 *
 * ⚠️ **extra_cookies 要传 `COOKIES_OS_IOS`，不要传 `COOKIES_OS_PC`**：post_common 的
 * 推导是 `appver = (os != "pc") ? "9.0.65" : ""`，用 pc 会发出 `appver=`（**空串**），
 * 实测与「压根不发 os/appver」同罪 —— 同端点同 br 只差这一对：
 * `check_music br=320000`（注入 os=ios+appver）拿到 url + freeTrialInfo，而
 * `song_url_old br=320000`（`COOKIES_OS_PC`）是 -110。mobile os + PC UA 与站内其它
 * 读请求本来就一致，不是新增的风险面。
 *
 * 注 1：url **不重写** —— 与 ne_call_weapi 一样，调用方要给已经带 /weapi/ 前缀的绝对 url。
 * 注 2：post_common **不塞** body 里的 `csrf_token`（那是 weapi_request 的活）。实测这条
 *       端点不要求它：2026-10-16 用带 os/appver 的 jar 直接调，没有 csrf_token 也拿到了
 *       url + freeTrialInfo。所以这里不为此改口径。 */
ne_resp *ne_call_weapi_cookies(const char *api, const jmap *data,
                               const char *const *extra_cookies) {
    ne_weapi_result enc;
    if (ne_weapi(data, &enc) != 0) return callweapi_encode_failed();
    const char *kv[4] = { "params", enc.params, "encSecKey", enc.enc_sec_key };
    char *form = ne_http_form_encode(kv, 2);
    /* orig_url 只被 post_common 用来判「要不要补 NMTID」（含 "login" 的 url），
     * 这里两者同一个 url 即正确。 */
    ne_resp *r = post_common(api, api, form, choose_ua(), extra_cookies, 0);
    free(form);
    ne_weapi_free(&enc);
    callweapi_validate_body(r);
    return r;
}

/* Shared weapi implementation. rewrite=1 → <...>api/ 段换成 /weapi/（绝大多数
 * 端点走这条）；rewrite=0 → url 原样发出（见 ne_create_weapi_asis）。 */
static ne_resp *weapi_request(const char *url, jmap *data,
                              const char *const *extra_cookies, int rewrite) {
    /* csrf_token from jar __csrf, injected pre-encryption (weapi branch).
     * Copied out under lock — jmap_put strdup's it immediately, but we must
     * not hold the pointer into the jar across the request. */
    char csrf[128];
    jar_lock();
    if (!g_jar) init_jar_locked();
    {
        const char *v = ne_jar_get(g_jar, "__csrf");
        snprintf(csrf, sizeof csrf, "%s", v ? v : "");
    }
    jar_unlock();
    jmap_put(data, "csrf_token", csrf);

    ne_weapi_result enc;
    if (ne_weapi(data, &enc) != 0) {
        ne_resp *r = ne_xmalloc(sizeof(ne_resp));
        r->code = 520; r->body = ne_xstrdup("encode failed");
        r->body_len = strlen(r->body); r->err = 1;
        return r;
    }
    const char *kv[4] = { "params", enc.params, "encSecKey", enc.enc_sec_key };
    char *form = ne_http_form_encode(kv, 2);
    char *final_url = rewrite ? ne_rewrite_api_segment(url, "/weapi/")
                              : ne_xstrdup(url);

    ne_resp *r = post_common(url, final_url, form, choose_ua(), extra_cookies, 0);
    free(form); free(final_url);
    ne_weapi_free(&enc);
    return r;
}

ne_resp *ne_create_weapi(const char *url, jmap *data,
                         const char *const *extra_cookies) {
    return weapi_request(url, data, extra_cookies, 1);
}

/* ne_create_weapi 的原样版：**不做 /api/ → /weapi/ 重写**，url 直接发出去。
 *
 * 用途：极少数端点的 /weapi/ 同名路由不可用，只能打 /api/ 老网关。目前唯一的
 * 已知场景是 block 流的 **cursor 分页**。2026-10-05 定测（同一份加密数据，随机
 * 化顺序、每次只换一个变量）：
 *   /api/homepage/block/page   + cursor → 200，136KB～146KB，正常
 *   /weapi/homepage/block/page + cursor → 200，**74 字节 code=50002**
 *   /weapi/homepage/block/page   不带 cursor / cursor 传空串 → 200，正常
 * cursor 的 JSON 类型（字符串 "-1" / 数字 -1 / 数字 0）、以及**服务端自己返回的
 * 那个真游标**，在 /weapi/ 下结果全是 50002；登录/未登录、os=ios/os=pc、refresh
 * 传布尔还是字符串也都一样；A/B 交替稳定跟随变量 → 不是风控、不是参数类型写错，
 * 而是 /weapi/ 那条路由**只实现了无分页形态**，老网关 /api/ 才认 cursor。
 *
 * 注意 ne_homepage_block_page **当前并不用它**：首页只要第一屏 blocks，不带
 * cursor 走标准 /weapi/ 就够，且这正是上游 api-enhanced 的默认调用形态
 * （它的 data.cursor 默认 undefined，序列化时被 JSON.stringify 丢掉）。
 * 将来真要按 cursor 翻 block 流，才需要本函数 + /api/ 前缀。
 *
 * 这类坑的表现极具误导性：探索页**雷达歌单整栏消失 + 猜你喜欢只剩登录提示**，
 * 与 UI 层 bug 长得一模一样（解析不出 data.blocks，两块内容一起空掉）。
 * 真机上唯一的线索是 blockPage 那行日志里的 len 小得反常（74）。 */
ne_resp *ne_create_weapi_asis(const char *url, jmap *data,
                              const char *const *extra_cookies) {
    return weapi_request(url, data, extra_cookies, 0);
}

/* CallWeapi-equivalent transport for login flows: no anti-fraud cookie
 * injection (no os/appver/NMTID), clean web request like request.go's
 * NewRequest path. */
ne_resp *ne_create_weapi_clean(const char *url, jmap *data) {
    char csrf[128];
    jar_lock();
    if (!g_jar) init_jar_locked();
    {
        const char *v = ne_jar_get(g_jar, "__csrf");
        snprintf(csrf, sizeof csrf, "%s", v ? v : "");
    }
    jar_unlock();
    jmap_put(data, "csrf_token", csrf);

    ne_weapi_result enc;
    if (ne_weapi(data, &enc) != 0) {
        ne_resp *r = ne_xmalloc(sizeof(ne_resp));
        r->code = 520; r->body = ne_xstrdup("encode failed");
        r->body_len = strlen(r->body); r->err = 1;
        return r;
    }
    const char *kv[4] = { "params", enc.params, "encSecKey", enc.enc_sec_key };
    char *form = ne_http_form_encode(kv, 2);
    char *final_url = ne_rewrite_api_segment(url, "/weapi/");

    ne_resp *r = post_common(url, final_url, form, choose_ua(), NULL, 1);
    free(form); free(final_url);
    ne_weapi_free(&enc);
    return r;
}

/* linuxapi branch: payload {method, url(/api/-rewritten), params} encrypted
 * with the linuxapi key, POSTed to /api/linux/forward with a Linux UA. */
static const char *UA_LINUX =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/60.0.3112.90 Safari/537.36";

ne_resp *ne_call_linuxapi(const char *url, jmap *data,
                          const char *const *extra_cookies) {
    char *api_url = ne_rewrite_api_segment(url, "/api/");

    jmap *outer = jmap_new();
    jmap_put(outer, "method", "POST");
    jmap_put(outer, "url", api_url);
    jmap_put_map(outer, "params", data);
    char *eparams = ne_linuxapi(outer);
    jmap_free(outer);
    free(api_url);
    if (!eparams) {
        ne_resp *r = ne_xmalloc(sizeof(ne_resp));
        r->code = 520; r->body = ne_xstrdup("encode failed");
        r->body_len = strlen(r->body); r->err = 1;
        return r;
    }

    char fwd[640];
    snprintf(fwd, sizeof fwd, "%s/api/linux/forward", api_base());
    const char *kv[2] = { "eparams", eparams };
    char *form = ne_http_form_encode(kv, 1);

    ne_resp *r = post_common(url, fwd, form, UA_LINUX, extra_cookies, 0);
    free(form); free(eparams);
    return r;
}

/* eapi branch (request.go:180-214): the mobile anti-fraud header object is
 * built from jar cookies + defaults, embedded as data["header"] AND sent as
 * request cookies; payload = AES-ECB(url path + json); URL /→/eapi/. */
static const char *jar_or(const ne_jar *j, const char *name,
                          const char *fallback) {
    const char *v = ne_jar_get(j, name);
    return (v && *v) ? v : fallback;
}

ne_resp *ne_call_eapi(const char *url, const char *eapi_path, jmap *data) {
    /* Locked snapshot: every pointer read here points into `snap`, our own
     * copy, so the extras kept alive across post_common() (which strdup's them
     * into its scratch) cannot dangle even if another thread writes g_jar. */
    ne_jar *snap = jar_snapshot();

    /* CookieValueByName(options.Cookies, name, fallback) — options.Cookies
     * for eapi is just the jar (no extras in any current service) */
    const char *os = jar_or(snap, "os", "ios");
    const char *appver = jar_or(snap, "appver",
                                strcmp(os, "pc") != 0 ? "9.0.65" : "");

    char buildver[32];
    snprintf(buildver, sizeof buildver, "%s", jar_or(snap, "buildver", ""));
    if (!buildver[0])
        snprintf(buildver, sizeof buildver, "%lld",
                 (long long)(ne_now_ms() / 1000));
    /* requestId = Unix()*1000 + rand(0..999) — seconds*1000, not real ms */
    char request_id[40];
    snprintf(request_id, sizeof request_id, "%lld%u",
             (long long)(ne_now_ms() / 1000) * 1000, ne_rand_below(1000));

    char *device_id = NULL;
    const char *dv = ne_jar_get(snap, "deviceId");
    if (!dv || !*dv) device_id = ne_random_device_id();
    const char *device_id_v = (dv && *dv) ? dv : device_id;

    const char *mu = ne_jar_get(snap, "MUSIC_U");
    const char *ma = ne_jar_get(snap, "MUSIC_A");

    jmap *header = jmap_new();
    jmap_put(header, "osver", jar_or(snap, "osver", "17.4.1"));
    jmap_put(header, "deviceId", device_id_v);
    jmap_put(header, "appver", appver);
    jmap_put(header, "versioncode", jar_or(snap, "versioncode", "140"));
    jmap_put(header, "mobilename", jar_or(snap, "mobilename", ""));
    jmap_put(header, "buildver", buildver);
    jmap_put(header, "resolution", jar_or(snap, "resolution", "1920x1080"));
    jmap_put(header, "__csrf", jar_or(snap, "__csrf", ""));
    jmap_put(header, "os", os);
    jmap_put(header, "channel", jar_or(snap, "channel", ""));
    jmap_put(header, "requestId", request_id);
    if (mu && *mu) jmap_put(header, "MUSIC_U", mu);
    if (ma && *ma) jmap_put(header, "MUSIC_A", ma);
    jmap_put_map(data, "header", header);

    char *params = ne_eapi(eapi_path, data);
    if (!params) {
        ne_jar_free(snap);
        free(device_id);
        ne_resp *r = ne_xmalloc(sizeof(ne_resp));
        r->code = 520; r->body = ne_xstrdup("encode failed");
        r->body_len = strlen(r->body); r->err = 1;
        return r;
    }

    const char *kv[2] = { "params", params };
    char *form = ne_http_form_encode(kv, 1);
    char *final_url = ne_rewrite_api_segment(url, "/eapi/");

    /* the header entries are ALSO sent as request cookies (request.go
     * SetCookies them after the options.Cookies loop) */
    const char *extras[32];
    int n = 0;
    extras[n++] = "osver";      extras[n++] = jar_or(snap, "osver", "17.4.1");
    extras[n++] = "deviceId";   extras[n++] = device_id_v;
    extras[n++] = "appver";     extras[n++] = appver;
    extras[n++] = "versioncode";extras[n++] = jar_or(snap, "versioncode", "140");
    extras[n++] = "mobilename"; extras[n++] = jar_or(snap, "mobilename", "");
    extras[n++] = "buildver";   extras[n++] = buildver;
    extras[n++] = "resolution"; extras[n++] = jar_or(snap, "resolution", "1920x1080");
    extras[n++] = "__csrf";     extras[n++] = jar_or(snap, "__csrf", "");
    extras[n++] = "os";         extras[n++] = os;
    extras[n++] = "channel";    extras[n++] = jar_or(snap, "channel", "");
    extras[n++] = "requestId";  extras[n++] = request_id;
    if (mu && *mu) { extras[n++] = "MUSIC_U"; extras[n++] = mu; }
    if (ma && *ma) { extras[n++] = "MUSIC_A"; extras[n++] = ma; }
    extras[n] = NULL;

    ne_resp *r = post_common(url, final_url, form, choose_ua(), extras, 0);
    ne_jar_free(snap);
    free(form); free(final_url); free(params); free(device_id);
    return r;
}

void ne_apply_request_strategy(void) {
    jar_lock();
    if (!g_jar) init_jar_locked();
    /* os=pc + fake NMTID — filterJar keeps the fake value off disk but
     * the jar-in-memory carries it, exactly like the Go process.
     * 2026-09: a server-issued NMTID (already in the jar) is never
     * overwritten — matching api-enhanced, which reuses the real value. */
    ne_jar_set(g_jar, "os", "pc");
    if (!ne_jar_get(g_jar, "NMTID") || !*ne_jar_get(g_jar, "NMTID"))
        ne_jar_set(g_jar, "NMTID", "some_random_id_from_strategy");
    jar_unlock();
}

char *ne_generate_chain_id(void) {
    jar_lock();
    if (!g_jar) init_jar_locked();
    const char *sd = ne_jar_get(g_jar, "sDeviceId");
    char *id;
    if (sd) {
        id = ne_xstrdup(sd);
    } else {
        static const char hexchars[] = "0123456789ABCDEF";
        id = ne_xmalloc(53);
        for (int i = 0; i < 52; i++) id[i] = hexchars[ne_rand_below(16)];
        id[52] = '\0';
    }
    jar_unlock();
    char *out = ne_xmalloc(64 + strlen(id));
    snprintf(out, 64 + strlen(id), "v1_%s_web_login_%lld", id,
             (long long)ne_now_ms());
    free(id);
    return out;
}

/* global jar accessor for the CLI (persist on exit). Returns the live jar
 * under lock; callers on the CLI use it single-threaded, and the returned
 * handle is only valid while no other request runs — safe for the CLI's
 * end-of-process persist. */
ne_jar *ne_global_jar(void) {
    jar_lock();
    if (!g_jar) init_jar_locked();
    ne_jar *jar = g_jar;
    jar_unlock();
    return jar;
}

/* Thread-safe merge of a browser-exported cookie string into the global jar
 * plus persist to the cookie file (used by the Android JNI importCookies path;
 * unlike raw ne_global_jar, safe to call concurrently with in-flight
 * requests). */
void ne_jar_import_cookies(const char *cookie_str) {
    if (!cookie_str || !*cookie_str) return;
    jar_lock();
    if (!g_jar) init_jar_locked();
    ne_jar_merge_cookie_str(g_jar, cookie_str);
    jar_persist_locked(1);   /* 导入必须落盘：不管指纹看起来变没变 */
    jar_unlock();
}
