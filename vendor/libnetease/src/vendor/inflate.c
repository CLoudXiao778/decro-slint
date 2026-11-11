/* DEFLATE decoder (RFC 1951) + zlib/gzip wrappers — see netease/inflate.h
 * for why this lives in vendor/ instead of linking libz.
 *
 * 结构：经典 canonical-Huffman 解码（count/symbol 表 + 逐位下降），
 * 输出**全量落在内存缓冲**里 —— 于是输出缓冲本身就是滑动窗口，
 * 回溯（back-reference）直接读自己，不需要单独维护 32 KiB 窗口。
 * 这是本实现能比通用 inflate 短一大截的原因，也是它只适合
 * 「一次性解压一个完整响应体」这个用途的原因。 */
#include "netease/inflate.h"
#include "netease/util.h"
#include <string.h>

#define MAXBITS 15

typedef struct {
    uint8_t *buf;
    size_t   len, cap, max;
    const uint8_t *in;
    size_t   in_len, in_pos;
    uint32_t bitbuf;
    int      bitcnt;
    int      err;      /* 一旦置位，后续所有按位读取都失效 */
} inflator;

/* ── 位流（DEFLATE 是 LSB-first 的位序）────────────────────────── */

static int bits(inflator *s, int need) {
    while (s->bitcnt < need) {
        if (s->in_pos >= s->in_len) { s->err = 1; return 0; }
        s->bitbuf |= (uint32_t)s->in[s->in_pos++] << s->bitcnt;
        s->bitcnt += 8;
    }
    int v = (int)(s->bitbuf & ((1u << need) - 1u));
    s->bitbuf >>= need;
    s->bitcnt -= need;
    return v;
}

/* ── 输出缓冲（超上限即失败，绝不无声截断）────────────────────── */

static void out_byte(inflator *s, uint8_t b) {
    if (s->len >= s->max) { s->err = 1; return; }
    if (s->len == s->cap) {
        size_t ncap = s->cap ? s->cap * 2 : 4096;
        if (ncap > s->max) ncap = s->max;
        s->buf = ne_xrealloc(s->buf, ncap);
        s->cap = ncap;
    }
    s->buf[s->len++] = b;
}

/* ── canonical Huffman ───────────────────────────────────────── */

typedef struct {
    short count[MAXBITS + 1];
    short symbol[288];
} huffman;

static void huff_build(huffman *h, const short *lengths, int n) {
    int i;
    for (i = 0; i <= MAXBITS; i++) h->count[i] = 0;
    for (i = 0; i < n; i++) h->count[lengths[i]]++;
    h->count[0] = 0;                       /* 长度为 0 = 该符号不存在 */

    short offs[MAXBITS + 2];
    offs[1] = 0;
    for (i = 1; i < MAXBITS; i++) offs[i + 1] = (short)(offs[i] + h->count[i]);
    for (i = 0; i < n; i++)
        if (lengths[i]) h->symbol[offs[lengths[i]]++] = (short)i;
}

static int huff_decode(inflator *s, const huffman *h) {
    int code = 0, first = 0, index = 0, len;
    for (len = 1; len <= MAXBITS; len++) {
        code |= bits(s, 1);
        if (s->err) return -1;
        int count = h->count[len];
        if (code - count < first)          /* 命中：canonical 区间内 */
            return h->symbol[index + (code - first)];
        index += count;
        first += count;
        first <<= 1;
        code <<= 1;
    }
    return -1;                              /* 15 位内没解出 → 码流非法 */
}

/* ── 块类型 ─────────────────────────────────────────────────── */

static int stored_block(inflator *s) {
    s->bitbuf = 0;                          /* 丢弃当前字节的剩余位 */
    s->bitcnt = 0;
    if (s->in_pos + 4 > s->in_len) return -1;
    unsigned len  = (unsigned)s->in[s->in_pos] | ((unsigned)s->in[s->in_pos + 1] << 8);
    unsigned nlen = (unsigned)s->in[s->in_pos + 2] | ((unsigned)s->in[s->in_pos + 3] << 8);
    if ((len ^ 0xffffu) != nlen) return -1; /* LEN/NLEN 互补校验 */
    s->in_pos += 4;
    if (s->in_pos + len > s->in_len) return -1;
    while (len--) out_byte(s, s->in[s->in_pos++]);
    return s->err ? -1 : 0;
}

static int codes(inflator *s, const huffman *lencode, const huffman *distcode) {
    static const short length_base[29] = {
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43,
        51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258
    };
    static const short length_extra[29] = {
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4,
        4, 4, 5, 5, 5, 5, 0
    };
    static const short dist_base[30] = {
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257,
        385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289,
        16385, 24577
    };
    static const short dist_extra[30] = {
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9,
        10, 10, 11, 11, 12, 12, 13, 13
    };

    for (;;) {
        int sym = huff_decode(s, lencode);
        if (sym < 0) return -1;
        if (sym < 256) {
            out_byte(s, (uint8_t)sym);
        } else if (sym == 256) {
            return s->err ? -1 : 0;         /* 结束码 */
        } else {
            sym -= 257;
            if (sym >= 29) return -1;       /* 286/287 是保留码 */
            int len = length_base[sym] + bits(s, length_extra[sym]);

            int dsym = huff_decode(s, distcode);
            if (dsym < 0 || dsym >= 30) return -1;
            int dist = dist_base[dsym] + bits(s, dist_extra[dsym]);
            if (s->err) return -1;
            if ((size_t)dist > s->len) return -1;   /* 引用到窗口之外 */

            /* 逐字节复制；dist 可以小于 len —— 这是 DEFLATE 允许的
             * 自重叠复制（压缩器用它表示重复串），必须按字节推进，
             * 不能用 memcpy。每次重新取 s->buf：out_byte 可能 realloc。 */
            size_t from = s->len - (size_t)dist;
            int i;
            for (i = 0; i < len; i++) out_byte(s, s->buf[from + (size_t)i]);
            if (s->err) return -1;
        }
    }
}

static int fixed_block(inflator *s) {
    short lengths[288];
    huffman lencode, distcode;
    int i = 0;
    for (; i < 144; i++) lengths[i] = 8;
    for (; i < 256; i++) lengths[i] = 9;
    for (; i < 280; i++) lengths[i] = 7;
    for (; i < 288; i++) lengths[i] = 8;
    huff_build(&lencode, lengths, 288);

    for (i = 0; i < 30; i++) lengths[i] = 5;
    huff_build(&distcode, lengths, 30);
    return codes(s, &lencode, &distcode);
}

static int dynamic_block(inflator *s) {
    /* code-length 字母表的**传输顺序**（不是字母序，照抄 RFC 1951 3.2.7） */
    static const short order[19] = {
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15
    };
    short lengths[320];
    huffman lencode, distcode;
    int i;

    int nlen  = bits(s, 5) + 257;            /* 字面/长度码数量 */
    int ndist = bits(s, 5) + 1;              /* 距离码数量 */
    int ncode = bits(s, 4) + 4;              /* code-length 码数量 */
    if (s->err) return -1;
    if (nlen > 286 || ndist > 30) return -1;

    for (i = 0; i < 19; i++) lengths[i] = 0;
    for (i = 0; i < ncode; i++) lengths[order[i]] = (short)bits(s, 3);
    if (s->err) return -1;
    huff_build(&lencode, lengths, 19);

    int index = 0;
    while (index < nlen + ndist) {
        int sym = huff_decode(s, &lencode);
        if (sym < 0) return -1;
        if (sym < 16) {
            lengths[index++] = (short)sym;
        } else {
            int repeat = 0, value = 0;
            if (sym == 16) {                 /* 重复上一个长度 3..6 次 */
                if (index == 0) return -1;
                value = lengths[index - 1];
                repeat = 3 + bits(s, 2);
            } else if (sym == 17) {          /* 0 重复 3..10 次 */
                repeat = 3 + bits(s, 3);
            } else {                         /* 18: 0 重复 11..138 次 */
                repeat = 11 + bits(s, 7);
            }
            if (s->err) return -1;
            if (index + repeat > nlen + ndist) return -1;
            while (repeat--) lengths[index++] = (short)value;
        }
    }

    if (lengths[256] == 0) return -1;        /* 没有结束码 = 解码会跑飞 */
    huff_build(&lencode, lengths, nlen);
    huff_build(&distcode, lengths + nlen, ndist);
    return codes(s, &lencode, &distcode);
}

static int inflate_blocks(inflator *s) {
    int last;
    do {
        last = bits(s, 1);
        int type = bits(s, 2);
        if (s->err) return -1;
        int r;
        if (type == 0)      r = stored_block(s);
        else if (type == 1) r = fixed_block(s);
        else if (type == 2) r = dynamic_block(s);
        else                return -1;       /* type 3 保留 */
        if (r != 0) return -1;
    } while (!last);
    return 0;
}

/* ── 包装层 ─────────────────────────────────────────────────── */

int ne_inflate_has_gzip_header(const uint8_t *in, size_t n) {
    return n >= 2 && in[0] == 0x1f && in[1] == 0x8b;
}

int ne_inflate_has_zlib_header(const uint8_t *in, size_t n) {
    if (n < 2) return 0;
    if ((in[0] & 0x0f) != 8) return 0;               /* CM 必须是 deflate */
    if ((((unsigned)in[0] << 8) | in[1]) % 31u != 0) return 0;  /* 头校验 */
    if (in[1] & 0x20) return 0;                      /* FDICT：预设字典，不支持 */
    return 1;
}

/* gzip 固定 10 字节头 + 可选的 EXTRA/NAME/COMMENT/HCRC 段 */
static int gzip_skip_header(inflator *s) {
    if (s->in_len < 10) return -1;
    if (s->in[2] != 8) return -1;                    /* CM = deflate */
    uint8_t flg = s->in[3];
    if (flg & 0xe0) return -1;                       /* 保留位必须为 0 */
    size_t p = 10;
    if (flg & 0x04) {                                /* FEXTRA */
        if (p + 2 > s->in_len) return -1;
        size_t xlen = (size_t)s->in[p] | ((size_t)s->in[p + 1] << 8);
        p += 2 + xlen;
        if (p > s->in_len) return -1;
    }
    if (flg & 0x08) {                                /* FNAME */
        while (p < s->in_len && s->in[p]) p++;
        if (p >= s->in_len) return -1;
        p++;
    }
    if (flg & 0x10) {                                /* FCOMMENT */
        while (p < s->in_len && s->in[p]) p++;
        if (p >= s->in_len) return -1;
        p++;
    }
    if (flg & 0x02) {                                /* FHCRC */
        p += 2;
        if (p > s->in_len) return -1;
    }
    s->in_pos = p;
    return 0;
}

static uint8_t *inflate_run(const uint8_t *in, size_t in_len, size_t skip,
                            size_t *out_len, size_t max_out) {
    inflator s;
    memset(&s, 0, sizeof s);
    s.in = in;
    s.in_len = in_len;
    s.in_pos = skip;
    s.max = max_out ? max_out : NE_INFLATE_DEFAULT_MAX;

    if (in_len <= skip) return NULL;
    if (inflate_blocks(&s) != 0) {
        free(s.buf);
        return NULL;
    }
    /* 空结果也算合法（真空输入）——但返回一个非 NULL 的 1 字节缓冲，
     * 让调用方能区分「解压成功且为空」与「失败」。 */
    if (!s.buf) s.buf = ne_xmalloc(1);
    *out_len = s.len;
    return s.buf;
}

uint8_t *ne_inflate_raw(const uint8_t *in, size_t in_len, size_t *out_len, size_t max_out) {
    if (!in || in_len == 0) return NULL;
    return inflate_run(in, in_len, 0, out_len, max_out);
}

uint8_t *ne_inflate(const uint8_t *in, size_t in_len, size_t *out_len, size_t max_out) {
    if (!in || in_len == 0) return NULL;
    if (ne_inflate_has_gzip_header(in, in_len)) {
        inflator probe;
        memset(&probe, 0, sizeof probe);
        probe.in = in;
        probe.in_len = in_len;
        if (gzip_skip_header(&probe) != 0) return NULL;
        return inflate_run(in, in_len, probe.in_pos, out_len, max_out);
    }
    if (ne_inflate_has_zlib_header(in, in_len))
        return inflate_run(in, in_len, 2, out_len, max_out);
    return NULL;                                     /* 无包装 → 交给调用方原样使用 */
}
