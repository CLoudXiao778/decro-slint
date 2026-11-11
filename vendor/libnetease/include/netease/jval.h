#ifndef NE_JVAL_H
#define NE_JVAL_H
#include <stddef.h>

/* jval: minimal JSON tree — what the Go shell gets from encoding/json.
 * Parse accepts any JSON document; marshal reproduces Go json.Marshal of
 * the equivalent map[string]interface{} value:
 *   - compact (no spaces), object keys bytewise sorted
 *   - Go string escaping (HTML escapes ON: & < > → \u0026 \u003c \u003e,
 *     plus U+2028/U+2029)
 *   - numbers: raw lexeme preserved (== Go's float64 round-trip for every
 *     value netease emits — ids/timestamps stay < 2^53)
 */

typedef enum {
    NE_JV_NULL = 0,
    NE_JV_BOOL,
    NE_JV_NUM,
    NE_JV_STR,
    NE_JV_ARR,
    NE_JV_OBJ
} ne_jv_type;

typedef struct ne_jval ne_jval;

struct ne_jval {
    ne_jv_type type;
    int b;              /* NE_JV_BOOL */
    char *num;          /* NE_JV_NUM: raw lexeme */
    char *str;          /* NE_JV_STR: decoded UTF-8 */
    ne_jval **items;    /* NE_JV_ARR / NE_JV_OBJ: values (insertion order) */
    char **keys;        /* NE_JV_OBJ only */
    size_t len, cap;
};

/* parse a complete JSON document; NULL on malformed input (Go Unmarshal
 * semantics: trailing garbage is an error) */
ne_jval *ne_jval_parse(const char *text);
/* Go encoding/json-style message of the last failed parse (static storage) —
 * feeds main.go's "parse account failed: %v" stderr line */
const char *ne_jval_last_error(void);

/* 流式顶层 "code" 提取（零节点分配，供 request.c 的 code 校验走捷径）：
 * 语义 = parse 后取根对象第一个 "code" 键且值为数字 → 1 并写 *out；
 * 其余（非法 JSON / 顶层非对象 / 无 code / 首个 code 非数字 / 尾随垃圾）
 * → 0。与树路径的唯一理论偏差：键名 "\u0063ode"（转义拼写）不匹配。 */
int ne_jval_scan_top_code(const char *text, double *out);

ne_jv_type ne_jval_type(const ne_jval *v);

/* object/array access — NULL when absent or wrong shape */
ne_jval *ne_jval_get(const ne_jval *obj, const char *key);
ne_jval *ne_jval_at(const ne_jval *arr, size_t i);
size_t ne_jval_len(const ne_jval *arr_or_obj);
const char *ne_jval_str(const ne_jval *v);  /* NULL unless NE_JV_STR */
double ne_jval_num(const ne_jval *v);       /* 0 unless NE_JV_NUM */
const char *ne_jval_num_lexeme(const ne_jval *v);  /* raw text, NULL unless NE_JV_NUM */
int ne_jval_bool(const ne_jval *v);         /* 0 unless NE_JV_BOOL && true */

/* builders — malloc'd; put/push take ownership of v */
ne_jval *ne_jval_new(ne_jv_type t);
ne_jval *ne_jval_new_str(const char *s);
ne_jval *ne_jval_new_num(const char *lexeme);
/* constructed double — Go float64 shortest form (codes are integral) */
ne_jval *ne_jval_new_num_d(double d);
ne_jval *ne_jval_new_bool(int b);
void ne_jval_put(ne_jval *obj, const char *key, ne_jval *v);
void ne_jval_push(ne_jval *arr, ne_jval *v);
ne_jval *ne_jval_clone(const ne_jval *v);
void ne_jval_free(ne_jval *v);

/* serialize Go json.Marshal style; malloc'd NUL-terminated */
char *ne_jval_marshal(const ne_jval *v);
#endif
