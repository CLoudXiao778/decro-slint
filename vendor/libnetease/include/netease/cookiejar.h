#ifndef NE_COOKIEJAR_H
#define NE_COOKIEJAR_H
#include <stddef.h>

/* In-memory cookie jar for https://music.163.com, persisted as Netscape
 * cookies.txt — byte-compatible with the Go shell's FileJar format:
 *   music.163.com\tFALSE\t/\tFALSE\t253402300799\t<name>\t<value>
 *
 * filterJar semantics (main.go): the anti-fraud strategy's FIXED fake NMTID
 * ("some_random_id_from_strategy") is never persisted — dropping it here
 * covers both the jar and the file. */

typedef struct ne_jar ne_jar;

ne_jar *ne_jar_new(void);
void ne_jar_free(ne_jar *j);

/* load/disk */
int ne_jar_load_file(ne_jar *j, const char *path);      /* tolerant */
int ne_jar_save_file(const ne_jar *j, const char *path);

/* update from a parsed "name=value" cookie; attribute names (Path, Domain,
 * Expires, ...) are rejected; fake NMTID rejected (filterJar). */
void ne_jar_set(ne_jar *j, const char *name, const char *value);

/* lookup; returns NULL if absent */
const char *ne_jar_get(const ne_jar *j, const char *name);

/* drop a cookie (no-op if absent). 删除语义的唯一入口 —— Set-Cookie 的
 * 过期下发与调用方手动清理都走它。 */
void ne_jar_remove(ne_jar *j, const char *name);

/* "k1=v1; k2=v2" Cookie header value (malloc'd) — jar order */
char *ne_jar_cookie_header(const ne_jar *j);

/* merge a "k=v; k2=v2" string into the jar (saveNeteaseCookies semantics:
 * filter attribute names + fake NMTID; here values are replaced in-memory) */
void ne_jar_merge_cookie_str(ne_jar *j, const char *cookie_str);

/* merge ONE raw Set-Cookie line, honouring its expiry attributes:
 *   Max-Age <= 0  或  Expires 已过去  →  删除该 cookie
 * 其余情况等同 ne_jar_set。**只有 Set-Cookie 行才有属性**，所以这条路径与
 * Cookie 头 / document.cookie 的合并分开（后者绝不会带 Max-Age/Expires，
 * 混在一起处理只会给那两条路径凭空加上删除能力）。
 * 服务端用 `Expires=Thu, 01 Jan 1970 00:00:00 GMT` 注销 cookie 是标准做法；
 * 不认这条属性，被注销的 cookie 会以「空值」形态永远留在 jar 与磁盘里，
 * 而且照样出现在每个请求的 Cookie 头上。 */
void ne_jar_merge_set_cookie(ne_jar *j, const char *set_cookie_line);
#endif
