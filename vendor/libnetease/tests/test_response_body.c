/* 响应体解码的确定性回归：压缩探测解压 + 按长度复制。
 *
 * 为什么需要它：这条路径在真机上**撞不到**（2026-10-07 实测：K40 冷启动 27 个
 * 请求 + 4 次写操作，服务端全程没发压缩体，也没有 Set-Cookie）。只看真机无法
 * 区分「代码对但没被触发」和「代码错且永远不触发」。这里装一个假传输层
 * （http.h 明确允许 ne_http_set_transport 注入），把压缩体直接喂进请求管线。
 *
 * 覆盖的判据：
 *   1. zlib / gzip 压缩的 JSON → 解出明文，且 code 正确解析；
 *   2. **业务错误码不被压成 200** —— 不解压的话 code 会落到「缺字段 → 200」的
 *      兜底上，用户会看到「收藏成功」而服务端其实回了 401（最坏的一类 bug）；
 *   3. 压缩的**非 JSON**（HTML 错误页）→ 不许解压（判据第三条：解出来必须像
 *      JSON），body 原样交给上层；
 *   4. 无包装的数据 → 原样，不猜；
 *   5. 假包装头（1f 8b 后面跟垃圾）→ 解压失败 → 原样，不崩；
 *   6. **按长度复制**：含 0x00 的响应体不许被 strlen 截断（压缩体里必然有
 *      0x00，这正是这条老路径一直没被发现的漏洞）；
 *   7. 空响应体 → 不崩。
 * 压缩体是离线生成的固定字节（见各数组），不依赖运行时的压缩库。 */
#include "netease/request.h"
#include "netease/http.h"
#include "netease/jmap.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

static const unsigned char *g_body;
static size_t g_body_len;

static ne_http_resp *fake_request(const char *url, const char *method,
                                  const char *body, const char *content_type,
                                  const char *cookie_header,
                                  const char *user_agent) {
    (void)url; (void)method; (void)body; (void)content_type;
    (void)cookie_header; (void)user_agent;
    ne_http_resp *r = (ne_http_resp *)malloc(sizeof *r);
    memset(r, 0, sizeof *r);
    r->status = 200;
    /* 按长度复制：压缩体里有 0x00，用 strdup 会当场截断 —— 这条测试本身
     * 就在盯着同一个坑。 */
    r->body = (char *)malloc(g_body_len + 1);
    if (g_body_len) memcpy(r->body, g_body, g_body_len);
    r->body[g_body_len] = '\0';
    r->body_len = g_body_len;
    return r;
}

static const ne_http_transport FAKE = { fake_request };

static ne_resp *fire(const unsigned char *body, size_t len) {
    g_body = body;
    g_body_len = len;
    jmap *m = jmap_new();
    ne_resp *r = ne_call_weapi("https://music.163.com/weapi/test", m);
    jmap_free(m);
    return r;
}

static void expect(int cond, const char *what) {
    if (cond) {
        printf("ok   %s\n", what);
    } else {
        printf("FAIL %s\n", what);
        failures++;
    }
}

/* ── 离线生成的压缩体（明文见各注释）────────────────────────── */

/* {"code":200,"result":{"songs":[1,2,3]}} —— 39 字节，含 0x00 */
static const unsigned char zlib_ok[] = {
    0x78,0xda,0xab,0x56,0x4a,0xce,0x4f,0x49,0x55,0xb2,0x32,0x32,0x30,0xd0,
    0x51,0x2a,0x4a,0x2d,0x2e,0xcd,0x29,0x51,0xb2,0xaa,0x56,0x2a,0xce,0xcf,
    0x4b,0x2f,0x56,0xb2,0x8a,0x36,0xd4,0x31,0xd2,0x31,0x8e,0xad,0xad,0x05,
    0x00,0xf2,0xdc,0x0c,0x33,
};
static const char *const zlib_ok_plain = "{\"code\":200,\"result\":{\"songs\":[1,2,3]}}";

static const unsigned char gzip_ok[] = {
    0x1f,0x8b,0x08,0x08,0x00,0x00,0x00,0x00,0x02,0xff,0x6f,0x6b,0x2e,0x74,
    0x78,0x74,0x00,0xab,0x56,0x4a,0xce,0x4f,0x49,0x55,0xb2,0x32,0x32,0x30,
    0xd0,0x51,0x2a,0x4a,0x2d,0x2e,0xcd,0x29,0x51,0xb2,0xaa,0x56,0x2a,0xce,
    0xcf,0x4b,0x2f,0x56,0xb2,0x8a,0x36,0xd4,0x31,0xd2,0x31,0x8e,0xad,0xad,
    0x05,0x00,0x68,0x82,0x5f,0x83,0x27,0x00,0x00,0x00,
};

/* {"code":401,"message":"下架歌曲无法收藏"} —— 业务失败藏在 200 的 body 里 */
static const unsigned char zlib_err[] = {
    0x78,0xda,0xab,0x56,0x4a,0xce,0x4f,0x49,0x55,0xb2,0x32,0x31,0x30,0xd4,
    0x51,0xca,0x4d,0x2d,0x2e,0x4e,0x4c,0x07,0xf2,0x94,0x9e,0xec,0xe8,0x7e,
    0x36,0x6f,0xdb,0xb3,0xb5,0x3d,0xcf,0x66,0x6f,0x7a,0x36,0x7d,0xc1,0xb3,
    0xcd,0x53,0x9f,0x4d,0xd9,0xf6,0x62,0x7a,0xbf,0x52,0x2d,0x00,0x04,0xc0,
    0x18,0xb6,
};
static const unsigned char gzip_err[] = {
    0x1f,0x8b,0x08,0x08,0x00,0x00,0x00,0x00,0x02,0xff,0x65,0x72,0x72,0x2e,
    0x74,0x78,0x74,0x00,0xab,0x56,0x4a,0xce,0x4f,0x49,0x55,0xb2,0x32,0x31,
    0x30,0xd4,0x51,0xca,0x4d,0x2d,0x2e,0x4e,0x4c,0x07,0xf2,0x94,0x9e,0xec,
    0xe8,0x7e,0x36,0x6f,0xdb,0xb3,0xb5,0x3d,0xcf,0x66,0x6f,0x7a,0x36,0x7d,
    0xc1,0xb3,0xcd,0x53,0x9f,0x4d,0xd9,0xf6,0x62,0x7a,0xbf,0x52,0x2d,0x00,
    0xe3,0x40,0x84,0x51,0x31,0x00,0x00,0x00,
};

/* <html><body>502 Bad Gateway</body></html> —— 网关错误页（非 JSON） */
static const unsigned char zlib_html[] = {
    0x78,0xda,0xb3,0xc9,0x28,0xc9,0xcd,0xb1,0xb3,0x49,0xca,0x4f,0xa9,0xb4,
    0x33,0x35,0x30,0x52,0x70,0x4a,0x4c,0x51,0x70,0x4f,0x2c,0x49,0x2d,0x4f,
    0xac,0xb4,0xd1,0x07,0x8b,0xda,0xe8,0x83,0x95,0x00,0x00,0x1d,0x80,0x0d,
    0xbd,
};

int main(void) {
    setvbuf(stdout, NULL, _IONBF, 0);
    ne_http_set_transport(&FAKE);

    /* ── 1. zlib 压缩的 JSON ───────────────────────────────── */
    {
        ne_resp *r = fire(zlib_ok, sizeof zlib_ok);
        expect(r->code == 200, "1 zlib 压缩体：code 正确解析");
        expect(r->body_len == strlen(zlib_ok_plain), "1 zlib 压缩体：body_len 是明文长度");
        expect(strcmp(r->body, zlib_ok_plain) == 0, "1 zlib 压缩体：解出原文");
        expect(strlen(r->body) == r->body_len, "1 解出的明文自身 NUL 结尾一致");
        ne_resp_free(r);
    }

    /* ── 2. gzip 压缩的 JSON ───────────────────────────────── */
    {
        ne_resp *r = fire(gzip_ok, sizeof gzip_ok);
        expect(r->code == 200 && strcmp(r->body, zlib_ok_plain) == 0,
               "2 gzip 压缩体：解出与 zlib 相同的明文");
        ne_resp_free(r);
    }

    /* ── 3. 压缩体里的业务错误码不许被读成 200 ──────────────── */
    {
        ne_resp *r = fire(zlib_err, sizeof zlib_err);
        expect(r->code == 401, "3 压缩体的 401 被正确读出（不解压会兜底成 200）");
        expect(strstr(r->body, "下架歌曲无法收藏") != NULL,
               "3 压缩体的 message 完整可见");
        ne_resp_free(r);
    }
    {
        ne_resp *r = fire(gzip_err, sizeof gzip_err);
        expect(r->code == 401, "3b gzip 包装的 401 同样正确");
        ne_resp_free(r);
    }

    /* ── 4. 压缩的 HTML：判据第三条（解出来必须像 JSON）救场 ── */
    {
        ne_resp *r = fire(zlib_html, sizeof zlib_html);
        expect(r->body_len == sizeof zlib_html,
               "4 非 JSON 的压缩体不被解压（body 原样交给上层）");
        ne_resp_free(r);
    }

    /* ── 5. 无包装 / 假包装：一律原样，不猜 ──────────────────── */
    {
        static const char plain[] = "{\"code\":200}";
        ne_resp *r = fire((const unsigned char *)plain, sizeof plain - 1);
        expect(r->code == 200 && strcmp(r->body, plain) == 0,
               "5 无包装的 JSON 原样通过");
        ne_resp_free(r);
    }
    {
        static const unsigned char fake_gzip[] = { 0x1f,0x8b,0x08,0x00,0xde,0xad,0xbe,0xef };
        ne_resp *r = fire(fake_gzip, sizeof fake_gzip);
        expect(r->body_len == sizeof fake_gzip, "5 假包装头解压失败 → 原样返回，不崩");
        ne_resp_free(r);
    }

    /* ── 6. 按长度复制：含 0x00 的响应体不许被 strlen 截断 ───── */
    {
        static const unsigned char with_nul[] = { '{','a',0x00,'b','}' };
        ne_resp *r = fire(with_nul, sizeof with_nul);
        expect(r->body_len == sizeof with_nul,
               "6 含 0x00 的响应体按长度保留（strlen 复制会截到 1 字节）");
        expect((unsigned char)r->body[2] == 0x00 && (unsigned char)r->body[4] == '}',
               "6 NUL 之后的内容仍在缓冲里");
        ne_resp_free(r);
    }

    /* ── 7. 空响应体 ──────────────────────────────────────── */
    {
        ne_resp *r = fire((const unsigned char *)"", 0);
        expect(r->body_len == 0 && r->body != NULL && r->body[0] == '\0',
               "7 空响应体：0 长度、可安全当字符串用");
        /* 注意这里**不是** 200 兜底：ne_call_weapi 走的是 CallWeapi 的严格语义
         * （request.go:375）—— body 必须能解出数字 code，否则 code=0 / err=2。
         * 「缺 code 就兜 200」是另一条宽松路径（CreateRequest / ne_create_weapi）
         * 的行为，两者别混。 */
        expect(r->code == 0 && r->err == 2,
               "7 空响应体走严格校验：code=0 / err=2（CallWeapi 语义，非 200 兜底）");
        ne_resp_free(r);
    }

    printf(failures ? "response_body: %d FAILURES\n" : "response_body: all ok\n", failures);
    return failures ? 1 : 0;
}
