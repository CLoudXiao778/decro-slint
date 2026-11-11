#ifndef NE_RISK_H
#define NE_RISK_H
#include "netease/request.h"

/* 风控状态分类 —— 对一次 API 响应给出可执行的风控语义。
 *
 * 网易云风控的典型表现(2024-2026 社区实证, 详见 docs/RISKS.md):
 *   - HTTP 200 + 空 body        → 数据中心/海外 IP 被直接丢弃(最隐蔽)
 *   - code -460 / 460 "Cheating" → 签名校验失败或频率限制(IP 或加密问题)
 *   - code -462 "网络太拥挤"     → 行为验证(滑块/验证码), 常伴随
 *                                  verifyType/verifyUrl 字段; 或 IP 太脏
 *   - code 8821                 → 手机号密码登录需行为验证码
 *   - HTTP 429/503/403          → 高频限流 / 网关拦截
 *   - code 301                  → 未登录 / 登录态过期
 * 这些信号是"先分类、再决策(退避/换 IP/引导浏览器)"的基础。 */

typedef enum {
    NE_RISK_OK = 0,            /* code 200 */
    NE_RISK_NEED_LOGIN,        /* 301: 未登录或登录态过期 */
    NE_RISK_BAD_REQUEST,       /* 400: 参数错误 */
    NE_RISK_CHEATING,          /* -460/460: 校验失败/疑似作弊/频率限制 */
    NE_RISK_VERIFY_REQUIRED,   /* -462/8821/verifyType: 需行为验证(滑块/验证码) */
    NE_RISK_HIGH_FREQ,         /* HTTP 429/503/403: 高频限流 */
    NE_RISK_IP_BLOCKED,        /* HTTP 200 + 空 body: 数据中心/海外 IP 被风控 */
    NE_RISK_TRANSPORT,         /* 传输层失败(超时/连接重置/520) */
    NE_RISK_UNKNOWN            /* 其他 */
} ne_risk_class;

/* 综合 HTTP 状态 + 业务 code + body 内容分类一次响应; r 为 NULL 时 UNKNOWN. */
ne_risk_class ne_risk_classify(const ne_resp *r);

/* 人类可读的原因描述(英文, 供日志/提示使用). */
const char *ne_risk_reason(ne_risk_class c);

/* 该分类是否值得退避后自动重试(-460/高频/传输错误为真; 行为验证/登录类为假). */
int ne_risk_is_transient(ne_risk_class c);

/* 指数退避 + 抖动: 第 attempt 次(0 起)重试前等待毫秒数, 上限 max_ms(默认 8s). */
int ne_risk_backoff_ms(int attempt, int max_ms);

/* 空 body 检测(数据中心 IP 风控的典型特征). */
int ne_risk_is_empty_body(const ne_resp *r);

/* 生成一个"看起来是国内出口"的随机 IPv4(活跃上游 Meting-API / api-enhanced
 * 同款做法, 用于 X-Real-IP / X-Forwarded-For 伪装以规避海外/数据中心 IP 风控).
 * 内置国内城市段表(58/59/60/61/112-125/202-223 等 /16 段), 段内随机.
 * buf 至少 16 字节. 仅供防御性使用, 无法保证与出口 IP 归属一致. */
void ne_random_cn_ip(char buf[16]);
#endif
