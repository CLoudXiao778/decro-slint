/* 语义等价对拍：ne_jval_scan_top_code vs 树解析(get "code")+NUM 判定。 */
#include "netease/jval.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

/* 树路径参照（= 改造前 parse_code/call_weapi 的真实行为） */
static int ref(const char *text, double *out) {
    ne_jval *root = ne_jval_parse(text);
    ne_jval *code = root && ne_jval_type(root) == NE_JV_OBJ
                  ? ne_jval_get(root, "code") : NULL;
    int ok = code && ne_jval_type(code) == NE_JV_NUM;
    if (ok) *out = ne_jval_num(code);
    ne_jval_free(root);
    return ok;
}

static unsigned rs = 12345;
static unsigned rnd(void) { rs = rs * 1103515245u + 12345u; return rs >> 8; }

/* 随机生成合法 JSON 文本：顶层强制对象，键池含 "code"（随机位置、随机
 * 值类型），字符串含普通转义（不含 \u——见 jval.c 文件头存档的理论偏差），
 * 数字含负/小数/指数。 */
static void gen_str(char *b) {
    static const char pool[] = "abcXYZ019 \\\\/\" \b\f\n\r\t _-";
    int n = rnd() % 12;
    *b++ = '"';
    for (int i = 0; i < n; i++) {
        char ch = pool[rnd() % (sizeof pool - 1)];
        if (ch == '\\' || ch == '"') { *b++ = '\\'; *b++ = ch; }
        else *b++ = ch;
    }
    *b++ = '"'; *b = 0;
}
static void gen_num(char *b) {
    switch (rnd() % 4) {
    case 0: sprintf(b, "%d", (int)(rnd() % 200000) - 100000); break;
    case 1: sprintf(b, "%d.%02d", (int)(rnd() % 300), (int)(rnd() % 100)); break;
    case 2: sprintf(b, "%de+%d", (int)(rnd() % 20), (int)(rnd() % 5)); break;
    case 3: strcpy(b, "0"); break;
    }
}
static void gen_val(char *b, int depth);
static void gen_val(char *b, int depth) {
    int t = rnd() % (depth > 3 ? 5 : 7);
    if (t == 0) { gen_str(b); }
    else if (t == 1) { gen_num(b); }
    else if (t < 5) { strcpy(b, t==2?"true":t==3?"false":"null"); }
    else {
        char obj = t == 5;
        char *p = b; *p++ = obj ? '{' : '[';
        int n = rnd() % 4;
        for (int i = 0; i < n; i++) {
            if (i) *p++ = ',';
            if (obj) { gen_str(p); p += strlen(p); *p++ = ':'; }
            gen_val(p, depth + 1); p += strlen(p);
        }
        *p++ = obj ? '}' : ']'; *p = 0;
    }
}
int main(void) {
    char buf[8192]; int bad = 0;
    /* 1) 随机对拍 20000 例 */
    for (int i = 0; i < 20000; i++) {
        char *p = buf; *p++ = '{';
        int n = 1 + rnd() % 5;
        for (int j = 0; j < n; j++) {
            if (j) *p++ = ',';
            if (rnd() % 3 == 0) { strcpy(p, "\"code\""); p += 6; }
            else { gen_str(p); p += strlen(p); }
            *p++ = ':';
            gen_val(p, 0); p += strlen(p);
        }
        *p++ = '}'; *p = 0;
        double a = -1, b = -1;
        int ra = ref(buf, &a), rb = ne_jval_scan_top_code(buf, &b);
        if (ra != rb || (ra && a != b)) {
            printf("MISMATCH: ref=%d(%.1f) scan=%d(%.1f)\n%s\n", ra, a, rb, b, buf);
            if (++bad > 5) return 1;
        }
    }
    /* 2) 固定边界：双方都必须判"无 code"（或同值） */
    const char *cases[] = {
        "{\"x\":1}",                          /* 无 code */
        "{\"code\":\"200\"}",                 /* code 非数字 */
        "{\"a\":{\"code\":200}}",             /* code 只在嵌套层 */
        "{\"a\":1}garbage",                   /* 尾随垃圾 */
        "{\"a\":1,",                          /* 截断 */
        "{\"a\":\"\\ud800\"}",                /* lone surrogate（转义体） */
        "{\"code\":200}{\"code\":200}",       /* 双文档（尾随垃圾变体） */
        "[{\"code\":200}]",                   /* 顶层非对象 */
        "\"just string\"",
        "{\"a\":\"str with \\\"code\\\": 1\"}",/* 值串里含 code 字样 */
        " { \"code\" : -460 , \"x\":1 } ",    /* 正常（ws 多样）+ 负值 */
        "{\"z\":9,\"code\":1e2}",             /* code 在后、科学计数 */
        "{\"code\":200,\"nested\":{\"deep\":[1,2,{\"code\":\"x\"}]}}",
    };
    for (size_t i = 0; i < sizeof cases / sizeof *cases; i++) {
        double a = -1, b = -1;
        int ra = ref(cases[i], &a), rb = ne_jval_scan_top_code(cases[i], &b);
        if (ra != rb || (ra && a != b)) {
            printf("CASE MISMATCH [%s]: ref=%d(%.1f) scan=%d(%.1f)\n",
                   cases[i], ra, a, rb, b);
            bad++;
        }
    }
    printf(bad ? "FAIL: %d mismatches\n" : "ALL PASS (20000 fuzz + %zu cases)\n",
           bad, sizeof cases / sizeof *cases);
    return bad ? 1 : 0;
}
