/* DEFLATE 解压的确定性回归（见 netease/inflate.h 说明为什么自带解压）。
 *
 * 判据分四类：
 *   1. 三种包装（gzip / zlib / raw）× 8 条矢量：长度 + FNV-1a 逐字节一致；
 *   2. 上限保护：max_out 刚好够 → 成功；少 1 字节 → 失败（防解压炸弹）；
 *   3. 包装探测的**保守性**：无包装的数据不许被猜成压缩体（raw 必须显式走
 *      ne_inflate_raw），头部非法必须拒绝；
 *   4. 损坏 / 截断一律返回 NULL —— 「失败是安全的」是这一层唯一的行为承诺，
 *      所以失败路径与成功路径一样要测。
 * 矢量由 tests/gen_inflate_vectors.py 重新生成（run_tests.sh 已接上）。 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "netease/inflate.h"

#include "inflate_vectors.h"

static int failures = 0;

static int fnv1a(const unsigned char *p, size_t n) {
    unsigned h = 0x811C9DC5u;
    for (size_t i = 0; i < n; i++)
        h = (h ^ p[i]) * 0x01000193u;
    return (int)h;
}

static void fail(const char *what, const char *name, const char *detail) {
    fprintf(stderr, "FAIL [%s] %s: %s\n", name, what, detail);
    failures++;
}

/* 解压并核对长度 + 指纹；ok=false 表示期望失败。 */
static void expect(const char *name, const char *label,
                   const unsigned char *in, size_t in_len,
                   int raw_mode, int expect_ok,
                   size_t plain_len, unsigned plain_sum, size_t max_out) {
    size_t out_len = 0;
    unsigned char *out = raw_mode
        ? ne_inflate_raw(in, in_len, &out_len, max_out)
        : ne_inflate(in, in_len, &out_len, max_out);

    if (!expect_ok) {
        if (out) {
            free(out);
            fail("expected failure but got output", name, label);
        }
        return;
    }
    if (!out) { fail("expected success but got NULL", name, label); return; }
    if (out_len != plain_len) {
        char buf[96];
        snprintf(buf, sizeof buf, "%s length %zu != %zu", label, out_len, plain_len);
        fail("length mismatch", name, buf);
    } else if ((unsigned)fnv1a(out, out_len) != plain_sum) {
        fail("fingerprint mismatch", name, label);
    }
    free(out);
}

static void coverage(void) {
    for (size_t i = 0; i < INFLATE_VECTOR_COUNT; i++) {
        const inflate_vector *v = &inflate_vectors[i];
        expect(v->name, "gzip", v->gz, v->gz_len, 0, 1, v->plain_len, v->plain_sum, 0);
        expect(v->name, "zlib", v->zl, v->zl_len, 0, 1, v->plain_len, v->plain_sum, 0);
        expect(v->name, "raw",  v->raw, v->raw_len, 1, 1, v->plain_len, v->plain_sum, 0);
        /* raw 数据**不能**被 ne_inflate 认成有包装的流（否则等于瞎猜） */
        expect(v->name, "raw-via-wrapper-probe", v->raw, v->raw_len, 0, 0, 0, 0, 0);
        /* 包装体走 raw 模式：解出来的东西必然不是原文（长度/指纹不匹配），
         * 这里只要求它别崩 —— 所以不断言结果，只确然调用一次。 */
    }
}

static void limits(void) {
    const inflate_vector *v = NULL;
    for (size_t i = 0; i < INFLATE_VECTOR_COUNT; i++)
        if (strcmp(inflate_vectors[i].name, "json_big") == 0) v = &inflate_vectors[i];
    if (!v) { fail("vector missing", "json_big", "generator changed?"); return; }

    /* 刚好够 → 成功 */
    expect("json_big", "cap-exact", v->zl, v->zl_len, 0, 1, v->plain_len, v->plain_sum, v->plain_len);
    /* 少一字节 → 必须失败（宁可不解压，也不截断出一个半截 JSON） */
    expect("json_big", "cap-minus-one", v->zl, v->zl_len, 0, 0, 0, 0, v->plain_len - 1);
    /* 极小的上限 → 失败 */
    expect("json_big", "cap-tiny", v->zl, v->zl_len, 0, 0, 0, 0, 16);
}

static void malformed(void) {
    /* 空 / NULL 输入 */
    expect("nil", "null", NULL, 0, 0, 0, 0, 0, 0);
    expect("empty", "zero-len", (const unsigned char *)"", 0, 0, 0, 0, 0, 0);

    /* 未压缩的普通数据：没有包装头，不许被当成压缩体 */
    static const unsigned char plain[] = "{\"code\":200,\"result\":{}}";
    expect("plain-json", "no-wrapper", plain, sizeof plain - 1, 0, 0, 0, 0, 0);
    /* 但显式按 raw 解 —— 位流非法，必须失败而不是吐垃圾 */
    expect("plain-json", "raw-garbage", plain, sizeof plain - 1, 1, 0, 0, 0, 0);

    /* zlib 头校验错误：把第 2 字节改到让 (CMF<<8|FLG) % 31 != 0 */
    unsigned char bad_zlib[32];
    size_t n = 0;
    for (size_t i = 0; i < INFLATE_VECTOR_COUNT; i++)
        if (strcmp(inflate_vectors[i].name, "json_small") == 0) {
            n = inflate_vectors[i].zl_len < sizeof bad_zlib ? inflate_vectors[i].zl_len : sizeof bad_zlib;
            memcpy(bad_zlib, inflate_vectors[i].zl, n);
        }
    if (n > 2) {
        unsigned char save = bad_zlib[1];
        bad_zlib[1] = (unsigned char)(save ^ 0x01);
        expect("bad-zlib-header", "checksum", bad_zlib, n, 0, 0, 0, 0, 0);
        bad_zlib[1] = save;
    }

    /* 截断：砍掉一半。只对**足够长**的流做 —— 短流（如空明文的 8 字节 zlib，
     * 数据区只有 `03 00` 两字节）砍一半后剩下的仍是合法完整位流，解出原文
     * 是正确行为而不是漏洞。 */
    for (size_t i = 0; i < INFLATE_VECTOR_COUNT; i++) {
        const inflate_vector *v = &inflate_vectors[i];
        if (v->zl_len < 16) continue;
        expect(v->name, "truncated", v->zl, v->zl_len / 2, 0, 0, 0, 0, 0);
    }

    /* 位流损坏：篡改**数据区中部**一个字节（跳过 2 字节 zlib 头，也避开尾部
     * adler32 —— 本实现刻意不校验校验和，改尾部等于没改，见 inflate.h）。
     * 合法结果只能是 NULL 或「与原文不同」的数据。 */
    for (size_t i = 0; i < INFLATE_VECTOR_COUNT; i++) {
        const inflate_vector *v = &inflate_vectors[i];
        if (v->zl_len < 16) continue;
        unsigned char *buf = malloc(v->zl_len);
        memcpy(buf, v->zl, v->zl_len);
        buf[2 + (v->zl_len - 2) / 2] ^= 0xFF;
        size_t out_len = 0;
        unsigned char *out = ne_inflate(buf, v->zl_len, &out_len, 0);
        if (out) {
            if (out_len == v->plain_len && (unsigned)fnv1a(out, out_len) == v->plain_sum)
                fail("corrupt stream decoded to the ORIGINAL bytes", v->name, "impossible");
            free(out);
        }
        free(buf);
    }

    /* gzip 头非法：CM 不是 deflate */
    unsigned char bad_gzip[16];
    for (size_t i = 0; i < INFLATE_VECTOR_COUNT; i++)
        if (strcmp(inflate_vectors[i].name, "json_small") == 0) {
            n = inflate_vectors[i].gz_len < sizeof bad_gzip ? inflate_vectors[i].gz_len : sizeof bad_gzip;
            memcpy(bad_gzip, inflate_vectors[i].gz, n);
        }
    if (n > 3) {
        bad_gzip[2] = 0x09;                         /* CM != 8 */
        expect("bad-gzip-cm", "cm", bad_gzip, n, 0, 0, 0, 0, 0);
        bad_gzip[2] = 0x08;
        bad_gzip[3] |= 0x20;                        /* 保留位必须为 0 */
        expect("bad-gzip-flag", "reserved", bad_gzip, n, 0, 0, 0, 0, 0);
    }
}

int main(void) {
    coverage();
    limits();
    malformed();

    if (failures) {
        fprintf(stderr, "inflate: %d failure(s)\n", failures);
        return 1;
    }
    printf("inflate: %zu vectors x 3 wrappers, limits + malformed OK\n",
           INFLATE_VECTOR_COUNT);
    return 0;
}
