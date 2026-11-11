/* jmap: ordered string map with Go json.Marshal(map[string]...) semantics. */
#include "netease/jmap.h"
#include "netease/util.h"
#include <string.h>
#include <stdio.h>

typedef enum { JV_STR, JV_MAP, JV_BOOL, JV_NUM } jv_kind;

typedef struct {
    char *key;
    jv_kind kind;
    char *str;     /* JV_STR */
    jmap *sub;     /* JV_MAP */
} jv;

struct jmap {
    jv *items;
    size_t len, cap;
};

jmap *jmap_new(void) {
    jmap *m = ne_xmalloc(sizeof(jmap));
    m->items = NULL; m->len = 0; m->cap = 0;
    return m;
}

/* binary search for key; returns index and *found */
static size_t jmap_find(const jmap *m, const char *key, int *found) {
    size_t lo = 0, hi = m->len;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        int c = strcmp(m->items[mid].key, key);
        if (c == 0) { *found = 1; return mid; }
        if (c < 0) lo = mid + 1; else hi = mid;
    }
    *found = 0;
    return lo;
}

static void jmap_insert(jmap *m, size_t pos, const char *key, jv_kind kind) {
    if (m->len == m->cap) {
        m->cap = m->cap ? m->cap * 2 : 8;
        m->items = ne_xrealloc(m->items, m->cap * sizeof(jv));
    }
    memmove(m->items + pos + 1, m->items + pos, (m->len - pos) * sizeof(jv));
    memset(&m->items[pos], 0, sizeof(jv));
    m->items[pos].key = ne_xstrdup(key);
    m->items[pos].kind = kind;
    m->len++;
}

static void jmap_set(jmap *m, const char *key, jv_kind kind) {
    int found;
    size_t pos = jmap_find(m, key, &found);
    if (found) {
        free(m->items[pos].str);
        if (m->items[pos].sub) jmap_free(m->items[pos].sub);
        m->items[pos].str = NULL;
        m->items[pos].sub = NULL;
        m->items[pos].kind = kind;
        return;
    }
    jmap_insert(m, pos, key, kind);
}

void jmap_put(jmap *m, const char *key, const char *val) {
    jmap_set(m, key, JV_STR);
    int found; (void)found;
    size_t pos = jmap_find(m, key, &found);
    m->items[pos].str = ne_xstrdup(val);
}

void jmap_put_map(jmap *m, const char *key, jmap *sub) {
    jmap_set(m, key, JV_MAP);
    int found; (void)found;
    size_t pos = jmap_find(m, key, &found);
    m->items[pos].sub = sub;
}

void jmap_put_bool(jmap *m, const char *key, int val) {
    jmap_set(m, key, JV_BOOL);
    int found; (void)found;
    size_t pos = jmap_find(m, key, &found);
    m->items[pos].str = ne_xstrdup(val ? "true" : "false");
}

void jmap_put_int(jmap *m, const char *key, long val) {
    jmap_set(m, key, JV_NUM);
    int found; (void)found;
    size_t pos = jmap_find(m, key, &found);
    char buf[24];
    snprintf(buf, sizeof(buf), "%ld", val);
    m->items[pos].str = ne_xstrdup(buf);
}

/* append JSON string with Go escaping:
 * `"`->\" `\`->\\ \n \r \t shortcuts, other <0x20 -> \u00xx,
 * '<' '>' '&' -> \u003c \u003e \u0026 (Go html-escape default ON) */
static void append_go_string(char **buf, size_t *len, size_t *cap,
                             const char *s) {
    static const char hexd[] = "0123456789abcdef";
#define PUTCH(c) do { \
        if (*len + 1 >= *cap) { *cap = *cap ? *cap * 2 : 64; \
            *buf = ne_xrealloc(*buf, *cap); } \
        (*buf)[(*len)++] = (char)(c); } while (0)
    PUTCH('"');
    for (const unsigned char *p = (const unsigned char *)s; *p; p++) {
        unsigned char c = *p;
        switch (c) {
            case '"':  PUTCH('\\'); PUTCH('"');  break;
            case '\\': PUTCH('\\'); PUTCH('\\'); break;
            case '\n': PUTCH('\\'); PUTCH('n');  break;
            case '\r': PUTCH('\\'); PUTCH('r');  break;
            case '\t': PUTCH('\\'); PUTCH('t');  break;
            case '<': case '>': case '&':
                PUTCH('\\'); PUTCH('u'); PUTCH('0'); PUTCH('0');
                PUTCH(hexd[c >> 4]); PUTCH(hexd[c & 15]);
                break;
            default:
                if (c < 0x20) {
                    PUTCH('\\'); PUTCH('u'); PUTCH('0'); PUTCH('0');
                    PUTCH(hexd[c >> 4]); PUTCH(hexd[c & 15]);
                } else {
                    PUTCH(c);
                }
        }
    }
    PUTCH('"');
#undef PUTCH
}

static void marshal_rec(const jmap *m, char **buf, size_t *len, size_t *cap) {
#define PUTS(s) do { const char *_s = (s); while (*_s) { \
        if (*len + 1 >= *cap) { *cap = *cap ? *cap * 2 : 64; \
            *buf = ne_xrealloc(*buf, *cap); } \
        (*buf)[(*len)++] = *_s++; } } while (0)
    PUTS("{");
    for (size_t i = 0; i < m->len; i++) {
        if (i) PUTS(",");
        append_go_string(buf, len, cap, m->items[i].key);
        PUTS(":");
        if (m->items[i].kind == JV_STR)
            append_go_string(buf, len, cap, m->items[i].str);
        else if (m->items[i].kind == JV_BOOL || m->items[i].kind == JV_NUM)
            PUTS(m->items[i].str);
        else
            marshal_rec(m->items[i].sub, buf, len, cap);
    }
    PUTS("}");
#undef PUTS
}

char *jmap_marshal(const jmap *m) {
    char *buf = NULL; size_t len = 0, cap = 0;
    marshal_rec(m, &buf, &len, &cap);
    if (!buf) { buf = ne_xmalloc(1); len = 0; }
    buf[len] = '\0';
    return buf;
}

void jmap_free(jmap *m) {
    if (!m) return;
    for (size_t i = 0; i < m->len; i++) {
        free(m->items[i].key);
        free(m->items[i].str);
        jmap_free(m->items[i].sub);
    }
    free(m->items);
    free(m);
}
