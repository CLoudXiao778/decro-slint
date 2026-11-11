#ifndef NE_JMAP_H
#define NE_JMAP_H

/* jmap: a string-keyed ordered map whose marshal() reproduces Go's
 * json.Marshal(map[string]...) byte-for-byte:
 *   - keys sorted bytewise ascending
 *   - string escaping: ", \, \n, \r, \t shortcuts; other <0x20 as \u00XX;
 *     HTML characters < > & escaped as \u003c \u003e \u0026 (Go default!)
 * This exactness matters: the ciphertext is a function of these bytes. */

typedef struct jmap jmap;

jmap *jmap_new(void);
/* put/replace a string value (copies both key and value) */
void jmap_put(jmap *m, const char *key, const char *val);
/* put/replace a JSON number (serialized without quotes) */
void jmap_put_int(jmap *m, const char *key, long val);
/* put/replace a JSON boolean (serialized as true/false without quotes) */
void jmap_put_bool(jmap *m, const char *key, int val);
/* put a nested object (takes ownership of `sub`) */
void jmap_put_map(jmap *m, const char *key, jmap *sub);
/* serialize to malloc'd JSON object text */
char *jmap_marshal(const jmap *m);
void jmap_free(jmap *m);
#endif
