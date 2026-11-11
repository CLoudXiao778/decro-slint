/* 风控状态分类与退避策略 (docs/RISKS.md 的代码化). */
#include "netease/risk.h"
#include "netease/rand.h"
#include "netease/util.h"
#include <stdio.h>
#include <string.h>

/* 国内主要运营商 /8 段(116-125、218-223 全段基本皆国内, 源自 Meting-API
 * 与 api-enhanced 的中国 IP 段表), 第二~四字节段内随机. 58-61/202-211 混
 * 有少量海外段, 故不纳入, 避免"看似国内"的 IP 反而暴露. */
static const unsigned char CN_IP_A[] = {
    116, 117, 118, 119, 120, 121, 122, 123, 124, 125, 218, 219, 220, 221, 222, 223
};

void ne_random_cn_ip(char buf[16]) {
    unsigned a = CN_IP_A[ne_rand_below((uint32_t)(sizeof CN_IP_A))];
    unsigned b = ne_rand_below(256);
    unsigned c = ne_rand_below(256);
    unsigned d = 1 + ne_rand_below(254);
    snprintf(buf, 16, "%u.%u.%u.%u", a, b, c, d);
}

int ne_risk_is_empty_body(const ne_resp *r) {
    return r && (!r->body || r->body_len == 0 || !*r->body);
}

ne_risk_class ne_risk_classify(const ne_resp *r) {
    if (!r) return NE_RISK_UNKNOWN;
    if (r->err == 1) return NE_RISK_TRANSPORT;      /* 传输层失败(520) */

    const long hs = r->http_status;
    if (hs == 429 || hs == 503) return NE_RISK_HIGH_FREQ;
    if (hs == 403) {
        /* 403 常与 WAF/IP 拦截相关; 空 body 视为 IP 风控 */
        return ne_risk_is_empty_body(r) ? NE_RISK_IP_BLOCKED : NE_RISK_HIGH_FREQ;
    }
    /* 数据中心 IP 风控的隐蔽特征: HTTP 200 + 0 字节 body */
    if (hs >= 200 && hs < 300 && ne_risk_is_empty_body(r))
        return NE_RISK_IP_BLOCKED;

    if (r->code == 200) return NE_RISK_OK;
    if (r->code == 301) return NE_RISK_NEED_LOGIN;
    if (r->code == 400) return NE_RISK_BAD_REQUEST;
    if (r->code == -460 || r->code == 460) return NE_RISK_CHEATING;
    if (r->code == -462 || r->code == 8821) return NE_RISK_VERIFY_REQUIRED;
    if (r->code == 503) return NE_RISK_HIGH_FREQ;
    if (r->code == 520) return NE_RISK_TRANSPORT;
    /* 附加启发: body 中出现验证字段即视为需行为验证 */
    if (r->body && (strstr(r->body, "verifyType") || strstr(r->body, "verifyUrl")))
        return NE_RISK_VERIFY_REQUIRED;
    return NE_RISK_UNKNOWN;
}

const char *ne_risk_reason(ne_risk_class c) {
    switch (c) {
    case NE_RISK_OK:             return "ok";
    case NE_RISK_NEED_LOGIN:     return "need login (301): login required or session expired";
    case NE_RISK_BAD_REQUEST:    return "bad request (400): invalid params";
    case NE_RISK_CHEATING:       return "risk-control -460: flagged cheating / rate-limited (check IP, headers, pacing)";
    case NE_RISK_VERIFY_REQUIRED:return "risk-control -462/8821: behavior verification (slider/captcha) required; use browser/QR login";
    case NE_RISK_HIGH_FREQ:      return "high-frequency (429/503/403): slow down, spread requests, rotate IP";
    case NE_RISK_IP_BLOCKED:     return "empty body (HTTP 200): datacenter/foreign IP blocked by risk control; use residential IP or X-Real-IP";
    case NE_RISK_TRANSPORT:      return "transport error (520): network timeout / connection reset";
    default:                     return "unknown";
    }
}

int ne_risk_is_transient(ne_risk_class c) {
    return c == NE_RISK_CHEATING || c == NE_RISK_HIGH_FREQ ||
           c == NE_RISK_TRANSPORT;
}

int ne_risk_backoff_ms(int attempt, int max_ms) {
    if (attempt < 1) attempt = 1;
    if (max_ms <= 0) max_ms = 8000;
    /* 200, 400, 800, 1600, ... 用循环倍增而非 200L<<(attempt-1), 避免
     * attempt 过大时移位溢出(UB, attempt 来自用户可配的 NE_RETRY_RISK) */
    long base = 200;
    for (int i = 1; i < attempt && base < max_ms; i++) base *= 2;
    if (base > max_ms) base = max_ms;
    long jitter = base / 4;                   /* ±25% 抖动 */
    long v = base;
    if (jitter > 0) v += (long)ne_rand_below((uint32_t)(jitter + 1));
    if (v > max_ms) v = max_ms;
    return (int)v;
}
