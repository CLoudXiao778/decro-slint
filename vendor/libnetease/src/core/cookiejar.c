/* Netscape cookies.txt jar + filterJar (see main.go) */
#include "netease/cookiejar.h"
#include "netease/util.h"
#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#ifdef _WIN32
#include <process.h>
#define NE_GETPID() ((int)_getpid())
#else
#include <unistd.h>
#define NE_GETPID() ((int)getpid())
#endif

#define NETEASE_HOST "music.163.com"
#define FAR_FUTURE   "253402300799"

typedef struct { char *name; char *value; } ne_cookie;

struct ne_jar {
    ne_cookie *items;
    size_t len, cap;
};

static int name_is_attr(const char *n) {
    /* RFC 6265 的属性名，外加早期草案里真在线上出现过的几个（version/comment/
     * discard/port/priority/partitioned）。Set-Cookie 是**线上格式**：值后面跟着
     * 一串属性，漏掉哪个名字，它就会被当成一个 cookie 存进 jar，之后又原样出现在
     * Cookie 请求头里（凭空多出一个假 cookie，对风控不是好事）。导入路径喂进来的
     * 是 document.cookie 形状（只有 k=v），碰不到这些。 */
    static const char *attrs[] = {
        "path", "domain", "expires", "max-age", "secure", "httponly", "samesite",
        "version", "comment", "commenturl", "discard", "port", "priority",
        "partitioned", NULL
    };
    for (int i = 0; attrs[i]; i++)
        if (strcasecmp(n, attrs[i]) == 0) return 1;
    return 0;
}

ne_jar *ne_jar_new(void) {
    ne_jar *j = ne_xmalloc(sizeof(ne_jar));
    j->items = NULL; j->len = 0; j->cap = 0;
    return j;
}

void ne_jar_free(ne_jar *j) {
    if (!j) return;
    for (size_t i = 0; i < j->len; i++) { free(j->items[i].name); free(j->items[i].value); }
    free(j->items);
    free(j);
}

static size_t jar_find(const ne_jar *j, const char *name) {
    for (size_t i = 0; i < j->len; i++)
        if (strcmp(j->items[i].name, name) == 0) return i;
    return (size_t)-1;
}

void ne_jar_set(ne_jar *j, const char *name, const char *value) {
    if (name_is_attr(name)) return;
    /* filterJar: the anti-fraud strategy's fixed fake NMTID never persists */
    if (strcasecmp(name, "NMTID") == 0 &&
        strcmp(value, "some_random_id_from_strategy") == 0) return;

    size_t idx = jar_find(j, name);
    if (idx != (size_t)-1) {
        free(j->items[idx].value);
        j->items[idx].value = ne_xstrdup(value);
        return;
    }
    if (j->len == j->cap) {
        j->cap = j->cap ? j->cap * 2 : 16;
        j->items = ne_xrealloc(j->items, j->cap * sizeof(ne_cookie));
    }
    j->items[j->len].name = ne_xstrdup(name);
    j->items[j->len].value = ne_xstrdup(value);
    j->len++;
}

const char *ne_jar_get(const ne_jar *j, const char *name) {
    size_t idx = jar_find(j, name);
    return idx == (size_t)-1 ? NULL : j->items[idx].value;
}

void ne_jar_remove(ne_jar *j, const char *name) {
    if (!j || !name || name_is_attr(name)) return;
    size_t idx = jar_find(j, name);
    if (idx == (size_t)-1) return;
    free(j->items[idx].name);
    free(j->items[idx].value);
    /* 尾部前移（保持顺序）：条目数量很小（实测登录后 14 条），memmove 的代价
     * 可以忽略，而保序能让落盘文件的内容稳定 —— 否则每次删除都会打乱
     * jar_persist_locked() 用来挡重复写盘的那个内容指纹。 */
    memmove(&j->items[idx], &j->items[idx + 1],
            (j->len - idx - 1) * sizeof(ne_cookie));
    j->len--;
}

int ne_jar_load_file(ne_jar *j, const char *path) {
    FILE *f = fopen(path, "r");
    if (!f) return -1;
    char line[4096];
    while (fgets(line, sizeof line, f)) {
        char *p = line;
        while (isspace((unsigned char)*p)) p++;
        if (*p == '\0' || *p == '#') continue;
        /* host \t include \t path \t secure \t expiry \t name \t value */
        char *fields[7] = {0};
        int nf = 0;
        char *tok = strtok(p, "\t\n");
        while (tok && nf < 7) { fields[nf++] = tok; tok = strtok(NULL, "\t\n"); }
        if (nf >= 7) ne_jar_set(j, fields[5], fields[6]);
    }
    fclose(f);
    return 0;
}

int ne_jar_save_file(const ne_jar *j, const char *path) {
    /* 先写**同目录**的临时文件，再 rename 顶替目标 —— 原实现是「以 wb 截断后
     * 直接写」，写盘途中进程被杀（Android 上系统回收、用户强杀）会留下半个
     * 文件，而登录态就丢在这半个文件里（USAGE §7 记录的已知残留，2026-10-10
     * 修）。rename 在同一文件系统内是原子的：读者要么看到旧的完整文件，要么
     * 看到新的完整文件，没有中间态。
     * 临时名带 pid：CLI 与 App 可能同时落盘，固定名会互相踩。 */
    size_t plen = strlen(path);
    char *tmp = ne_xmalloc(plen + 32);
    snprintf(tmp, plen + 32, "%s.tmp%d", path, NE_GETPID());

    /* 二进制模式不是可选项：文本模式下 MSVC 会把 \n 落成 \r\n，文件就不再是
     * cookiejar.h 承诺的 Go FileJar 字节格式（同一份 jar 在不同平台上不一致，
     * 跨平台读回时值里还会带上行尾残留）。 */
    FILE *f = fopen(tmp, "wb");
    if (!f) { free(tmp); return -1; }
    fprintf(f, "# Netscape HTTP Cookie File\n");
    for (size_t i = 0; i < j->len; i++)
        fprintf(f, "%s\tFALSE\t/\tFALSE\t%s\t%s\t%s\n",
                NETEASE_HOST, FAR_FUTURE, j->items[i].name, j->items[i].value);
    /* fclose 的返回值必须看：写失败（磁盘满）常常延迟到 flush/close 才报，
     * 漏掉它就等于把「写坏了」当成「写好了」—— jar_persist_locked() 会据此
     * 记下指纹，此后再也不补写（见它对写失败的处理）。 */
    if (fclose(f) != 0) { remove(tmp); free(tmp); return -1; }

#ifdef _WIN32
    /* Windows 的 rename 对已存在的目标会失败（POSIX 是覆盖）。先删再改名，
     * 窗口只有一次 unlink，比「截断后写」的整个写盘窗口小得多。 */
    remove(path);
#endif
    if (rename(tmp, path) != 0) { remove(tmp); free(tmp); return -1; }
    free(tmp);
    return 0;
}

char *ne_jar_cookie_header(const ne_jar *j) {
    size_t cap = 64, len = 0;
    char *buf = ne_xmalloc(cap);
    buf[0] = '\0';
#define APP(s) do { size_t _l = strlen(s); \
        while (len + _l + 2 > cap) { cap *= 2; buf = ne_xrealloc(buf, cap); } \
        memcpy(buf + len, s, _l); len += _l; buf[len] = 0; } while (0)
    for (size_t i = 0; i < j->len; i++) {
        if (i) APP("; ");
        APP(j->items[i].name); APP("="); APP(j->items[i].value);
    }
#undef APP
    return buf;
}

void ne_jar_merge_cookie_str(ne_jar *j, const char *cookie_str) {
    char *copy = ne_xstrdup(cookie_str);
    char *save = NULL;
    for (char *part = strtok_r(copy, ";", &save); part; part = strtok_r(NULL, ";", &save)) {
        while (isspace((unsigned char)*part)) part++;
        char *eq = strchr(part, '=');
        if (!eq) continue;
        *eq = '\0';
        ne_jar_set(j, part, eq + 1);   /* set() filters attrs + fake NMTID */
    }
    free(copy);
}

/* ── Set-Cookie 单行：带过期语义 ─────────────────────────────────── */

/* 公历日期 → Unix 秒（自实现，不用 timegm/_mkgmtime）。
 *
 * 两个理由：① 这两个函数跨平台可用性不一（MSVC 叫 _mkgmtime，bionic 起步较晚）；
 * ② timegm 的行为受 TZ 环境影响（某些平台上它只是「假装 UTC 的 mktime」），而
 * cookie 过期判断必须与运行环境的时区无关 —— 差一小时倒不至于误删，但把
 * 「已过期」算成「未过期」会让注销的 cookie 留在 jar 里。
 * 算法是 Howard Hinnant 的 days_from_civil（公历 → 天数，含闰年修正）。 */
static long long days_from_civil(long long y, int m, int d) {
    y -= (m <= 2);
    long long era = (y >= 0 ? y : y - 399) / 400;
    long long yoe = y - era * 400;                         /* [0, 399] */
    long long doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
    long long doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    return era * 146097 + doe - 719468;
}

/* "Wed, 21 Oct 2015 07:28:00 GMT"（RFC 1123）与无星期的变体；两位年份
 * 按 RFC 850 惯例折算。成功返回 0 并写出 Unix 秒。 */
static int parse_http_date(const char *s, long long *out) {
    static const char *months[12] = {
        "Jan", "Feb", "Mar", "Apr", "May", "Jun",
        "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"
    };
    int d = 0, y = 0, hh = 0, mm = 0, ss = 0;
    char wday[8] = {0}, mon[8] = {0};

    while (*s == ' ' || *s == '\t') s++;
    int n = sscanf(s, "%7s %d %7s %d %d:%d:%d", wday, &d, mon, &y, &hh, &mm, &ss);
    if (n != 7) {
        n = sscanf(s, "%d %7s %d %d:%d:%d", &d, mon, &y, &hh, &mm, &ss);
        if (n != 6) return -1;
    }
    int mi = -1;
    for (int i = 0; i < 12; i++)
        if (strncasecmp(mon, months[i], 3) == 0) { mi = i; break; }
    if (mi < 0 || d <= 0 || hh < 0 || mm < 0 || ss < 0) return -1;
    if (y < 100) y += (y < 70) ? 2000 : 1900;      /* 两位年份 */

    *out = days_from_civil(y, mi + 1, d) * 86400LL
         + (long long)hh * 3600 + (long long)mm * 60 + ss;
    return 0;
}

/* 扫描属性段，判断这条 Set-Cookie 是否表示「删除」。
 *
 * RFC 6265 §4.1.2：Max-Age 存在时优先于 Expires；Max-Age <= 0 即删除。
 * 属性解析不出来（格式怪、日期非法）时**一律不删** —— 保守方向必须是
 * 「留着」，因为误删 MUSIC_U 的代价是用户被登出，而误留一个空值 cookie
 * 只是多几字节。 */
static int set_cookie_says_delete(const char *line, size_t name_len) {
    /* 只看 name=value 之后的部分，避免值里恰好含 "max-age" 字样 */
    const char *p = line + name_len;
    const char *semi = strchr(p, ';');
    if (!semi) return 0;

    int has_max_age = 0;
    long long max_age = 0;
    int has_expires = 0;
    long long expires = 0;

    char *copy = ne_xstrdup(semi + 1);
    char *save = NULL;
    for (char *a = strtok_r(copy, ";", &save); a; a = strtok_r(NULL, ";", &save)) {
        while (isspace((unsigned char)*a)) a++;
        char *eq = strchr(a, '=');
        if (!eq) continue;
        size_t klen = (size_t)(eq - a);
        while (klen > 0 && isspace((unsigned char)a[klen - 1])) klen--;
        const char *v = eq + 1;
        while (isspace((unsigned char)*v)) v++;

        if (klen == 7 && strncasecmp(a, "Max-Age", 7) == 0) {
            char *endp = NULL;
            max_age = strtoll(v, &endp, 10);
            /* 解析不出数字（空值 / 垃圾）→ 当作没有这条属性，而不是当成 0。
             * 把「读不懂」判成「删除」迟早会误删 MUSIC_U。 */
            has_max_age = (endp != v);
        } else if (klen == 7 && strncasecmp(a, "Expires", 7) == 0) {
            has_expires = 1;
            if (parse_http_date(v, &expires) != 0) has_expires = 0;
        }
    }
    free(copy);

    if (has_max_age) return max_age <= 0;
    if (has_expires) return expires <= (long long)time(NULL);
    return 0;
}

void ne_jar_merge_set_cookie(ne_jar *j, const char *line) {
    if (!line || !*line) return;

    const char *s = line;
    while (*s == ' ' || *s == '\t') s++;
    const char *end = s;
    while (*end && *end != '=' && *end != ';') end++;
    size_t name_len = (size_t)(end - s);
    if (name_len == 0) return;                     /* 没有 name，无事可做 */

    char name[256];
    if (name_len >= sizeof name) return;           /* 病态长度，丢弃 */
    memcpy(name, s, name_len);
    name[name_len] = '\0';

    if (set_cookie_says_delete(line, name_len))
        ne_jar_remove(j, name);
    else
        ne_jar_merge_cookie_str(j, line);          /* 复用处：过滤属性与假 NMTID */
}
