/* jval: JSON tree parser + Go json.Marshal-style serializer. */
#include "netease/jval.h"
#include "netease/util.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define NE_MAX_DEPTH 128

/* ── growable byte buffer ──────────────────────────────── */
typedef struct {
    char *s;
    size_t len, cap;
} sbuf;

static void sb_reserve(sbuf *b, size_t extra) {
    if (b->len + extra + 1 > b->cap) {
        b->cap = b->cap ? b->cap : 64;
        while (b->len + extra + 1 > b->cap) b->cap *= 2;
        b->s = ne_xrealloc(b->s, b->cap);
    }
}
static void sb_putc(sbuf *b, char c) {
    sb_reserve(b, 1);
    b->s[b->len++] = c;
}
static void sb_puts(sbuf *b, const char *s) {
    while (*s) sb_putc(b, *s++);
}
static char *sb_finish(sbuf *b) {
    if (!b->s) { b->s = ne_xmalloc(1); b->len = 0; }
    b->s[b->len] = '\0';
    return b->s;
}

/* ── constructors ──────────────────────────────────────── */
static ne_jval *jv(ne_jv_type t) {
    ne_jval *v = ne_xmalloc(sizeof *v);
    memset(v, 0, sizeof *v);
    v->type = t;
    return v;
}

ne_jval *ne_jval_new(ne_jv_type t) { return jv(t); }

ne_jval *ne_jval_new_str(const char *s) {
    ne_jval *v = jv(NE_JV_STR);
    v->str = ne_xstrdup(s);
    return v;
}

ne_jval *ne_jval_new_num(const char *lexeme) {
    ne_jval *v = jv(NE_JV_NUM);
    v->num = ne_xstrdup(lexeme);
    return v;
}

ne_jval *ne_jval_new_num_d(double d) {
    char buf[40];
    /* Go floatEncoder: strconv 'f'/-1 — integral values in our range print
     * without exponent or fraction */
    if (d == (double)(long long)d && fabs(d) < 1e15)
        snprintf(buf, sizeof buf, "%.0f", d);
    else
        snprintf(buf, sizeof buf, "%.17g", d);
    return ne_jval_new_num(buf);
}

ne_jval *ne_jval_new_bool(int b) {
    ne_jval *v = jv(NE_JV_BOOL);
    v->b = !!b;
    return v;
}

static void grow_items(ne_jval *c) {
    if (c->len == c->cap) {
        c->cap = c->cap ? c->cap * 2 : 8;
        c->items = ne_xrealloc(c->items, c->cap * sizeof(ne_jval *));
        if (c->type == NE_JV_OBJ)
            c->keys = ne_xrealloc(c->keys, c->cap * sizeof(char *));
    }
}

/* put/replace (Go map assignment semantics) — takes ownership of v */
void ne_jval_put(ne_jval *obj, const char *key, ne_jval *v) {
    for (size_t i = 0; i < obj->len; i++) {
        if (strcmp(obj->keys[i], key) == 0) {
            ne_jval_free(obj->items[i]);
            obj->items[i] = v;
            return;
        }
    }
    grow_items(obj);
    obj->keys[obj->len] = ne_xstrdup(key);
    obj->items[obj->len] = v;
    obj->len++;
}

void ne_jval_push(ne_jval *arr, ne_jval *v) {
    grow_items(arr);
    arr->items[arr->len++] = v;
}

ne_jval *ne_jval_clone(const ne_jval *v) {
    if (!v) return NULL;
    ne_jval *c = jv(v->type);
    c->b = v->b;
    if (v->num) c->num = ne_xstrdup(v->num);
    if (v->str) c->str = ne_xstrdup(v->str);
    for (size_t i = 0; i < v->len; i++) {
        grow_items(c);
        if (v->type == NE_JV_OBJ) c->keys[i] = ne_xstrdup(v->keys[i]);
        c->items[i] = ne_jval_clone(v->items[i]);
        c->len++;
    }
    return c;
}

void ne_jval_free(ne_jval *v) {
    if (!v) return;
    for (size_t i = 0; i < v->len; i++) {
        if (v->type == NE_JV_OBJ) free(v->keys[i]);
        ne_jval_free(v->items[i]);
    }
    free(v->items);
    free(v->keys);
    free(v->num);
    free(v->str);
    free(v);
}

/* ── accessors ─────────────────────────────────────────── */
ne_jv_type ne_jval_type(const ne_jval *v) { return v ? v->type : NE_JV_NULL; }

ne_jval *ne_jval_get(const ne_jval *obj, const char *key) {
    if (!obj || obj->type != NE_JV_OBJ) return NULL;
    for (size_t i = 0; i < obj->len; i++)
        if (strcmp(obj->keys[i], key) == 0) return obj->items[i];
    return NULL;
}

ne_jval *ne_jval_at(const ne_jval *arr, size_t i) {
    if (!arr || arr->type != NE_JV_ARR || i >= arr->len) return NULL;
    return arr->items[i];
}

size_t ne_jval_len(const ne_jval *c) {
    if (!c || (c->type != NE_JV_ARR && c->type != NE_JV_OBJ)) return 0;
    return c->len;
}

const char *ne_jval_str(const ne_jval *v) {
    return v && v->type == NE_JV_STR ? v->str : NULL;
}

double ne_jval_num(const ne_jval *v) {
    return v && v->type == NE_JV_NUM ? strtod(v->num, NULL) : 0.0;
}

const char *ne_jval_num_lexeme(const ne_jval *v) {
    return v && v->type == NE_JV_NUM ? v->num : NULL;
}

int ne_jval_bool(const ne_jval *v) {
    return v && v->type == NE_JV_BOOL ? v->b : 0;
}

/* ── parser (recursive descent) ────────────────────────── */
typedef struct {
    const char *p;
    int depth;
    char err[192];   /* Go encoding/json-style SyntaxError message */
} pctx;

/* the message of the most recent failed ne_jval_parse ("parse account
 * failed: %v" — main.go prints it verbatim on stderr) */
static char ne_jval_errbuf[192];

const char *ne_jval_last_error(void) { return ne_jval_errbuf; }

/* Go's quoteChar: printable → 'x', control → '\xNN' (used inside
 * "invalid character ..." messages) */
static void set_err_invalid_char(pctx *c, char ch, const char *ctx) {
    unsigned char u = (unsigned char)ch;
    if (u >= 0x20 && u < 0x7F)
        snprintf(c->err, sizeof c->err, "invalid character '%c'%s%s",
                 ch, ctx ? " " : "", ctx ? ctx : "");
    else
        snprintf(c->err, sizeof c->err, "invalid character '\\x%02x'%s%s",
                 u, ctx ? " " : "", ctx ? ctx : "");
}

static ne_jval *parse_value(pctx *c);

static void skip_ws(pctx *c) {
    while (*c->p == ' ' || *c->p == '\t' || *c->p == '\n' || *c->p == '\r')
        c->p++;
}

static int hex_digit(char ch) {
    if (ch >= '0' && ch <= '9') return ch - '0';
    if (ch >= 'a' && ch <= 'f') return ch - 'a' + 10;
    if (ch >= 'A' && ch <= 'F') return ch - 'A' + 10;
    return -1;
}

static unsigned hex4(const char *p) {  /* caller validated 4 hex digits */
    unsigned v = 0;
    for (int i = 0; i < 4; i++) v = v * 16 + (unsigned)hex_digit(p[i]);
    return v;
}

static void utf8_put(sbuf *b, unsigned cp) {
    if (cp < 0x80) {
        sb_putc(b, (char)cp);
    } else if (cp < 0x800) {
        sb_putc(b, (char)(0xC0 | (cp >> 6)));
        sb_putc(b, (char)(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
        sb_putc(b, (char)(0xE0 | (cp >> 12)));
        sb_putc(b, (char)(0x80 | ((cp >> 6) & 0x3F)));
        sb_putc(b, (char)(0x80 | (cp & 0x3F)));
    } else {
        sb_putc(b, (char)(0xF0 | (cp >> 18)));
        sb_putc(b, (char)(0x80 | ((cp >> 12) & 0x3F)));
        sb_putc(b, (char)(0x80 | ((cp >> 6) & 0x3F)));
        sb_putc(b, (char)(0x80 | (cp & 0x3F)));
    }
}

/* cursor sits ON the opening quote; NULL on malformed string */
static char *parse_string_raw(pctx *c) {
    c->p++;  /* skip '"' */
    sbuf b = {0};
    while (*c->p && *c->p != '"') {
        if (*c->p == '\\') {
            c->p++;
            switch (*c->p) {
                case '"':  sb_putc(&b, '"');  c->p++; break;
                case '\\': sb_putc(&b, '\\'); c->p++; break;
                case '/':  sb_putc(&b, '/');  c->p++; break;
                case 'b':  sb_putc(&b, '\b'); c->p++; break;
                case 'f':  sb_putc(&b, '\f'); c->p++; break;
                case 'n':  sb_putc(&b, '\n'); c->p++; break;
                case 'r':  sb_putc(&b, '\r'); c->p++; break;
                case 't':  sb_putc(&b, '\t'); c->p++; break;
                case 'u': {
                    int ok = 1;
                    for (int i = 1; i <= 4; i++)
                        if (hex_digit(c->p[i]) < 0) { ok = 0; break; }
                    if (!ok) {
                        set_err_invalid_char(c, c->p[1] ? c->p[1] : '\0',
                                             "in \\u hexadecimal character escape");
                        free(b.s);
                        return NULL;
                    }
                    unsigned cp = hex4(c->p + 1);
                    c->p += 5;
                    if (cp >= 0xD800 && cp <= 0xDBFF) {
                        /* high surrogate must be paired — Go rejects
                         * lone surrogates ("illegal UTF-16 surrogate") */
                        int lo_ok = c->p[0] == '\\' && c->p[1] == 'u';
                        if (lo_ok)
                            for (int i = 2; i <= 5; i++)
                                if (hex_digit(c->p[i]) < 0) { lo_ok = 0; break; }
                        unsigned lo = lo_ok ? hex4(c->p + 2) : 0;
                        if (!lo_ok || lo < 0xDC00 || lo > 0xDFFF) {
                            snprintf(c->err, sizeof c->err,
                                     "illegal UTF-16 surrogate");
                            free(b.s);
                            return NULL;
                        }
                        cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                        c->p += 6;
                    } else if (cp >= 0xDC00 && cp <= 0xDFFF) {
                        snprintf(c->err, sizeof c->err,
                                 "illegal UTF-16 surrogate");
                        free(b.s);   /* lone low surrogate */
                        return NULL;
                    }
                    utf8_put(&b, cp);
                    break;
                }
                case '\0':
                    snprintf(c->err, sizeof c->err,
                             "unexpected end of JSON input");
                    free(b.s);
                    return NULL;
                default:
                    set_err_invalid_char(c, *c->p, "in string escape code");
                    free(b.s);
                    return NULL;
            }
        } else {
            sb_putc(&b, *c->p++);
        }
    }
    if (*c->p != '"') {
        snprintf(c->err, sizeof c->err, "unexpected end of JSON input");
        free(b.s);
        return NULL;
    }
    c->p++;
    return sb_finish(&b);
}

static ne_jval *parse_number(pctx *c) {
    const char *start = c->p;
    if (*c->p == '-') c->p++;
    if (!(*c->p >= '0' && *c->p <= '9')) {
        set_err_invalid_char(c, *c->p ? *c->p : '\0', "in numeric literal");
        return NULL;
    }
    if (*c->p == '0') {
        c->p++;
    } else {
        while (*c->p >= '0' && *c->p <= '9') c->p++;
    }
    if (*c->p == '.') {
        c->p++;
        if (!(*c->p >= '0' && *c->p <= '9')) {
            set_err_invalid_char(c, *c->p ? *c->p : '\0', "after decimal point in numeric literal");
            return NULL;
        }
        while (*c->p >= '0' && *c->p <= '9') c->p++;
    }
    if (*c->p == 'e' || *c->p == 'E') {
        c->p++;
        if (*c->p == '+' || *c->p == '-') c->p++;
        if (!(*c->p >= '0' && *c->p <= '9')) {
            set_err_invalid_char(c, *c->p ? *c->p : '\0', "in exponent of numeric literal");
            return NULL;
        }
        while (*c->p >= '0' && *c->p <= '9') c->p++;
    }
    ne_jval *v = jv(NE_JV_NUM);
    v->num = ne_xmalloc((size_t)(c->p - start) + 1);
    memcpy(v->num, start, (size_t)(c->p - start));
    v->num[c->p - start] = '\0';
    return v;
}

/* "true"/"false"/"null" — Go scanner: EOF mid-literal → unexpected end;
 * wrong char → "invalid character 'x' in literal true (expecting 'e')" */
static ne_jval *parse_literal(pctx *c, const char *lit, ne_jv_type t, int b) {
    for (const char *q = lit; *q; q++, c->p++) {
        if (*c->p == '\0') {
            snprintf(c->err, sizeof c->err, "unexpected end of JSON input");
            return NULL;
        }
        if (*c->p != *q) {
            set_err_invalid_char(c, *c->p, NULL);
            char tail[64];
            snprintf(tail, sizeof tail, " in literal %s (expecting '%c')",
                     lit, *q);
            strcat(c->err, tail);
            return NULL;
        }
    }
    return t == NE_JV_BOOL ? ne_jval_new_bool(b) : jv(NE_JV_NULL);
}

static ne_jval *parse_value(pctx *c) {
    if (c->depth >= NE_MAX_DEPTH) {
        snprintf(c->err, sizeof c->err, "exceeded max depth");
        return NULL;
    }
    skip_ws(c);
    char ch = *c->p;
    if (ch == '{') {
        c->p++;
        ne_jval *o = jv(NE_JV_OBJ);
        skip_ws(c);
        if (*c->p == '}') { c->p++; return o; }
        for (;;) {
            skip_ws(c);
            if (*c->p != '"') {
                if (*c->p == '\0')
                    snprintf(c->err, sizeof c->err, "unexpected end of JSON input");
                else
                    set_err_invalid_char(c, *c->p, "looking for beginning of object key string");
                ne_jval_free(o);
                return NULL;
            }
            char *key = parse_string_raw(c);
            if (!key) { ne_jval_free(o); return NULL; }
            skip_ws(c);
            if (*c->p != ':') {
                if (*c->p == '\0')
                    snprintf(c->err, sizeof c->err, "unexpected end of JSON input");
                else
                    set_err_invalid_char(c, *c->p, "after object key");
                free(key);
                ne_jval_free(o);
                return NULL;
            }
            c->p++;
            c->depth++;
            ne_jval *val = parse_value(c);
            c->depth--;
            if (!val) { free(key); ne_jval_free(o); return NULL; }
            grow_items(o);
            o->keys[o->len] = key;
            o->items[o->len] = val;
            o->len++;
            skip_ws(c);
            if (*c->p == ',') { c->p++; continue; }
            if (*c->p == '}') { c->p++; return o; }
            if (*c->p == '\0')
                snprintf(c->err, sizeof c->err, "unexpected end of JSON input");
            else
                set_err_invalid_char(c, *c->p, "after object key:value pair");
            ne_jval_free(o);
            return NULL;
        }
    }
    if (ch == '[') {
        c->p++;
        ne_jval *a = jv(NE_JV_ARR);
        skip_ws(c);
        if (*c->p == ']') { c->p++; return a; }
        for (;;) {
            c->depth++;
            ne_jval *val = parse_value(c);
            c->depth--;
            if (!val) { ne_jval_free(a); return NULL; }
            ne_jval_push(a, val);
            skip_ws(c);
            if (*c->p == ',') { c->p++; continue; }
            if (*c->p == ']') { c->p++; return a; }
            if (*c->p == '\0')
                snprintf(c->err, sizeof c->err, "unexpected end of JSON input");
            else
                set_err_invalid_char(c, *c->p, "after array element");
            ne_jval_free(a);
            return NULL;
        }
    }
    if (ch == '"') {
        char *s = parse_string_raw(c);
        if (!s) return NULL;
        ne_jval *v = jv(NE_JV_STR);
        v->str = s;
        return v;
    }
    if (ch == 't') return parse_literal(c, "true", NE_JV_BOOL, 1);
    if (ch == 'f') return parse_literal(c, "false", NE_JV_BOOL, 0);
    if (ch == 'n') return parse_literal(c, "null", NE_JV_NULL, 0);
    if (ch == '-' || (ch >= '0' && ch <= '9')) return parse_number(c);
    if (ch == '\0')
        snprintf(c->err, sizeof c->err, "unexpected end of JSON input");
    else
        set_err_invalid_char(c, ch, "looking for beginning of value");
    return NULL;
}

ne_jval *ne_jval_parse(const char *text) {
    ne_jval_errbuf[0] = '\0';
    if (!text) {
        snprintf(ne_jval_errbuf, sizeof ne_jval_errbuf,
                 "unexpected end of JSON input");
        return NULL;
    }
    pctx c = { text, 0, {0} };
    ne_jval *v = parse_value(&c);
    if (!v) {
        snprintf(ne_jval_errbuf, sizeof ne_jval_errbuf, "%s", c.err);
        return NULL;
    }
    skip_ws(&c);
    if (*c.p != '\0') {  /* trailing garbage — Go Unmarshal rejects */
        set_err_invalid_char(&c, *c.p, "after top-level value");
        snprintf(ne_jval_errbuf, sizeof ne_jval_errbuf, "%s", c.err);
        ne_jval_free(v);
        return NULL;
    }
    return v;
}

/* ── serializer (Go json.Marshal of interface{} values) ── */

/* Go string escaping: control-char shortcuts, \u00XX, HTML escapes,
 * U+2028/U+2029. UTF-8 passthrough for everything else. */
static void append_go_string(sbuf *b, const char *s) {
    static const char hexd[] = "0123456789abcdef";
    sb_putc(b, '"');
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        unsigned char ch = *p;
        switch (ch) {
            case '"':  sb_puts(b, "\\\""); break;
            case '\\': sb_puts(b, "\\\\"); break;
            case '\n': sb_puts(b, "\\n");  break;
            case '\r': sb_puts(b, "\\r");  break;
            case '\t': sb_puts(b, "\\t");  break;
            case '<': case '>': case '&':
                sb_puts(b, "\\u00");
                sb_putc(b, hexd[ch >> 4]);
                sb_putc(b, hexd[ch & 15]);
                break;
            case 0xE2:  /* U+2028 / U+2029 (JS separators) */
                if (p[1] == 0x80 && (p[2] == 0xA8 || p[2] == 0xA9)) {
                    sb_puts(b, p[2] == 0xA8 ? "\\u2028" : "\\u2029");
                    p += 2;
                    break;
                }
                /* fall through */
            default:
                if (ch < 0x20) {
                    sb_puts(b, "\\u00");
                    sb_putc(b, hexd[ch >> 4]);
                    sb_putc(b, hexd[ch & 15]);
                } else {
                    sb_putc(b, (char)ch);
                }
        }
    }
    sb_putc(b, '"');
}

typedef struct {
    const char *k;
    const ne_jval *v;
} kvpair;

static int kvcmp(const void *a, const void *b) {
    return strcmp(((const kvpair *)a)->k, ((const kvpair *)b)->k);
}

static void marshal_rec(const ne_jval *v, sbuf *b) {
    if (!v) { sb_puts(b, "null"); return; }
    switch (v->type) {
        case NE_JV_NULL: sb_puts(b, "null"); break;
        case NE_JV_BOOL: sb_puts(b, v->b ? "true" : "false"); break;
        case NE_JV_NUM:  sb_puts(b, v->num ? v->num : "0"); break;
        case NE_JV_STR:  append_go_string(b, v->str ? v->str : ""); break;
        case NE_JV_ARR:
            sb_putc(b, '[');
            for (size_t i = 0; i < v->len; i++) {
                if (i) sb_putc(b, ',');
                marshal_rec(v->items[i], b);
            }
            sb_putc(b, ']');
            break;
        case NE_JV_OBJ: {
            /* Go marshals map keys bytewise sorted — sort an index view,
             * the tree itself stays in insertion order */
            kvpair *pairs = ne_xmalloc(v->len * sizeof(kvpair));
            for (size_t i = 0; i < v->len; i++) {
                pairs[i].k = v->keys[i];
                pairs[i].v = v->items[i];
            }
            if (v->len > 1) qsort(pairs, v->len, sizeof(kvpair), kvcmp);
            sb_putc(b, '{');
            for (size_t i = 0; i < v->len; i++) {
                if (i) sb_putc(b, ',');
                append_go_string(b, pairs[i].k);
                sb_putc(b, ':');
                marshal_rec(pairs[i].v, b);
            }
            sb_putc(b, '}');
            free(pairs);
            break;
        }
    }
}

char *ne_jval_marshal(const ne_jval *v) {
    sbuf b = {0};
    marshal_rec(v, &b);
    return sb_finish(&b);
}

/* ── 流式顶层 "code" 提取（性能路径，2026-10） ────────────────────
 * 语义对齐 parse_code 的树路径：ne_jval_parse → 根为 OBJ → get("code")
 * （第一个匹配键）→ 值为 NUM → 该数值；其余一切（非法 JSON、顶层非对象、
 * 无 code、第一个 code 非数字、尾随垃圾）→ 无 code。
 * 背景：358KB 真实响应建全树 ~11ms（节点逐 malloc），本扫描 <1ms 且零
 * 分配；request.c 的 finish/CallWeapi 校验各用一次 = 每请求省两次全树。
 * 严格性与树解析一致：控制字符、字符串转义文法、数字文法、surrogate
 * 配对、尾随垃圾全校验。已知唯一理论偏差：键名写作 "\u0063ode"（解码后
 * 才是 "code"）时扫描不匹配——真实响应不出现，注释存档。 */
typedef struct { const char *p; } sctx;

static void s_ws(sctx *c) {
    while (*c->p == ' ' || *c->p == '\t' || *c->p == '\n' || *c->p == '\r')
        c->p++;
}

static int s_hexdigit(char ch) {
    return (ch >= '0' && ch <= '9') || (ch >= 'a' && ch <= 'f') ||
           (ch >= 'A' && ch <= 'F');
}

/* ON 开引号；成功时越过闭引号，失败 0。转义文法与 parse_string_raw 对齐
 * （含 surrogate 配对校验，保证"解析失败→无 code"的判定两侧一致）。 */
static int s_skip_string(sctx *c) {
    c->p++;
    for (;;) {
        unsigned char ch = (unsigned char)*c->p;
        if (ch == '"') { c->p++; return 1; }
        if (ch == '\0') return 0;   /* 树语义：裸控制字符宽容接受，NUL=意外结束 */
        if (ch == '\\') {
            c->p++;
            switch (*c->p) {
            case '"': case '\\': case '/': case 'b': case 'f':
            case 'n': case 'r': case 't':
                c->p++; break;
            case 'u': {
                for (int i = 1; i <= 4; i++)
                    if (!s_hexdigit(c->p[i])) return 0;
                unsigned cp = 0;
                for (int i = 1; i <= 4; i++)
                    cp = cp * 16 + (unsigned)hex_digit(c->p[i]);
                c->p += 5;
                if (cp >= 0xD800 && cp <= 0xDBFF) {
                    if (c->p[0] == '\\' && c->p[1] == 'u') {
                        unsigned lo = 0; int ok = 1;
                        for (int i = 2; i <= 5; i++)
                            if (!s_hexdigit(c->p[i])) { ok = 0; break; }
                        if (ok)
                            for (int i = 2; i <= 5; i++)
                                lo = lo * 16 + (unsigned)hex_digit(c->p[i]);
                        if (ok && lo >= 0xDC00 && lo <= 0xDFFF) {
                            c->p += 6;
                            break;
                        }
                    }
                    return 0;   /* lone high surrogate */
                }
                if (cp >= 0xDC00 && cp <= 0xDFFF) return 0; /* lone low */
                break;
            }
            default: return 0;
            }
        } else {
            c->p++;
        }
    }
}

/* 严格 JSON 数字文法；成功时 *start 指向词素起点。 */
static int s_skip_number(sctx *c, const char **start) {
    const char *s = c->p;
    if (*c->p == '-') c->p++;
    if (*c->p == '0') c->p++;
    else if (*c->p >= '1' && *c->p <= '9') {
        while (*c->p >= '0' && *c->p <= '9') c->p++;
    } else return 0;
    if (*c->p == '.') {
        c->p++;
        if (!(*c->p >= '0' && *c->p <= '9')) return 0;
        while (*c->p >= '0' && *c->p <= '9') c->p++;
    }
    if (*c->p == 'e' || *c->p == 'E') {
        c->p++;
        if (*c->p == '+' || *c->p == '-') c->p++;
        if (!(*c->p >= '0' && *c->p <= '9')) return 0;
        while (*c->p >= '0' && *c->p <= '9') c->p++;
    }
    *start = s;
    return 1;
}

static int s_skip_value(sctx *c, int depth);

#define S_MAX_DEPTH 128   /* = NE_MAX_DEPTH：树路径深于此即失败 -> 无 code */

static int s_skip_container(sctx *c, int depth, char open, char close) {
    c->p++;
    for (;;) {
        s_ws(c);
        if (*c->p == close) { c->p++; return 1; }
        if (open == '{') {                      /* 对象：先键后冒号 */
            if (*c->p != '"') return 0;
            if (!s_skip_string(c)) return 0;
            s_ws(c);
            if (*c->p != ':') return 0;
            c->p++;
        }
        if (!s_skip_value(c, depth + 1)) return 0;
        s_ws(c);
        if (*c->p == ',') { c->p++; continue; }
        if (*c->p == close) { c->p++; return 1; }
        return 0;
    }
}

static int s_skip_value(sctx *c, int depth) {
    if (depth >= S_MAX_DEPTH) return 0;   /* 树：parse_value 入口判 NE_MAX_DEPTH */
    s_ws(c);
    switch (*c->p) {
    case '{': return s_skip_container(c, depth, '{', '}');
    case '[': return s_skip_container(c, depth, '[', ']');
    case '"': return s_skip_string(c);
    case 't': return strncmp(c->p, "true", 4) == 0 ? (c->p += 4, 1) : 0;
    case 'f': return strncmp(c->p, "false", 5) == 0 ? (c->p += 5, 1) : 0;
    case 'n': return strncmp(c->p, "null", 4) == 0 ? (c->p += 4, 1) : 0;
    default: {
        const char *start;
        return s_skip_number(c, &start);
    }
    }
}

int ne_jval_scan_top_code(const char *text, double *out) {
    if (!text) return 0;
    sctx c = { text };
    s_ws(&c);
    if (*c.p != '{') return 0;
    c.p++;
    for (;;) {
        s_ws(&c);
        if (*c.p == '}') { c.p++; break; }
        if (*c.p != '"') return 0;
        /* 原字节匹配 "code"（纯 ASCII 键无转义形式；"\u0063ode" 见文件头注释） */
        if (strncmp(c.p + 1, "code\"", 5) == 0) {
            c.p += 6;
            s_ws(&c);
            if (*c.p != ':') return 0;
            c.p++;
            s_ws(&c);
            const char *start;
            if (!(*c.p == '-' || (*c.p >= '0' && *c.p <= '9'))) return 0;
            if (!s_skip_number(&c, &start)) return 0;
            /* 剩余顶层成员仍需完整校验（含尾随垃圾），失败 = 整体无 code */
            double v = strtod(start, NULL);
            for (;;) {
                s_ws(&c);
                if (*c.p == '}') { c.p++; goto done; }
                if (*c.p != ',') return 0;
                c.p++;
                s_ws(&c);
                if (*c.p != '"') return 0;
                if (!s_skip_string(&c)) return 0;
                s_ws(&c);
                if (*c.p != ':') return 0;
                c.p++;
                if (!s_skip_value(&c, 1)) return 0;
            }
        done:
            s_ws(&c);
            if (*c.p != '\0') return 0;    /* 尾随垃圾（Go Unmarshal 语义） */
            *out = v;
            return 1;
        }
        if (!s_skip_string(&c)) return 0;
        s_ws(&c);
        if (*c.p != ':') return 0;
        c.p++;
        if (!s_skip_value(&c, 1)) return 0;
        s_ws(&c);
        if (*c.p == ',') { c.p++; continue; }
        if (*c.p == '}') { c.p++; break; }
        return 0;
    }
    s_ws(&c);
    if (*c.p != '\0') return 0;
    return 0;
}
