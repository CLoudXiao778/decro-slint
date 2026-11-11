/* jval tests — Go encoding/json semantics (sorted keys, compact, HTML
 * escaping, lexeme-preserving numbers, surrogate decoding). */
#include "netease/jval.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

static void check_str(const char *name, const char *got, const char *want) {
    if (!got || strcmp(got, want) != 0) {
        fprintf(stderr, "FAIL %s\n  got:  %s\n  want: %s\n",
                name, got ? got : "(null)", want);
        failures++;
    } else {
        printf("ok %s\n", name);
    }
}

static void check_int(const char *name, long long got, long long want) {
    if (got != want) {
        fprintf(stderr, "FAIL %s: got %lld want %lld\n", name, got, want);
        failures++;
    } else {
        printf("ok %s\n", name);
    }
}

/* parse → marshal round trip against the Go-expected canonical text */
static void roundtrip(const char *name, const char *in, const char *want) {
    ne_jval *v = ne_jval_parse(in);
    if (!v) {
        fprintf(stderr, "FAIL %s: parse returned NULL for %s\n", name, in);
        failures++;
        return;
    }
    char *out = ne_jval_marshal(v);
    check_str(name, out, want);
    free(out);
    ne_jval_free(v);
}

static void must_fail(const char *name, const char *in) {
    ne_jval *v = ne_jval_parse(in);
    if (v) {
        fprintf(stderr, "FAIL %s: accepted malformed %s\n", name, in);
        ne_jval_free(v);
        failures++;
    } else {
        printf("ok %s\n", name);
    }
}

/* parse fails AND the message matches Go encoding/json's %v exactly —
 * main.go feeds these into "parse account failed: %v" stderr lines */
static void must_fail_msg(const char *name, const char *in, const char *want) {
    ne_jval *v = ne_jval_parse(in);
    if (v) {
        fprintf(stderr, "FAIL %s: accepted malformed %s\n", name, in);
        ne_jval_free(v);
        failures++;
        return;
    }
    check_str(name, ne_jval_last_error(), want);
}

int main(void) {
    roundtrip("sorted keys + compact",
              "{\"b\":1,\"a\":2}", "{\"a\":2,\"b\":1}");
    roundtrip("html escape",
              "{\"u\":\"a&b<c>d\"}", "{\"u\":\"a\\u0026b\\u003cc\\u003ed\"}");
    roundtrip("bmp escape decodes to utf8",
              "{\"k\":\"\\u4f60\\u597d\"}", "{\"k\":\"你好\"}");
    roundtrip("surrogate pair",
              "{\"e\":\"\\ud83d\\ude00\"}", "{\"e\":\"😀\"}");
    roundtrip("array order preserved",
              "[3,1,{\"z\":0,\"a\":true}]", "[3,1,{\"a\":true,\"z\":0}]");
    roundtrip("number lexemes preserved",
              "[1.50,1e3,-0,0.5]", "[1.50,1e3,-0,0.5]");
    roundtrip("string escapes round trip",
              "{\"s\":\"line\\n\\\"q\\\"\\t\\\\\"}",
              "{\"s\":\"line\\n\\\"q\\\"\\t\\\\\"}");
    roundtrip("literals",
              "{\"n\":null,\"t\":true,\"f\":false}",
              "{\"f\":false,\"n\":null,\"t\":true}");
    roundtrip("nested containers",
              "{\"x\":[{\"b\":1,\"a\":[2,1]}]}",
              "{\"x\":[{\"a\":[2,1],\"b\":1}]}");
    roundtrip("empty containers", "{}", "{}");
    roundtrip("empty array", "[]", "[]");

    must_fail("trailing garbage", "{}x");
    must_fail("unclosed object", "{\"a\":1");
    must_fail("trailing comma", "[1,]");
    must_fail("empty input", "");
    must_fail("bad literal", "tru");
    must_fail("lone surrogate high", "\"\\ud800\"");
    must_fail("lone surrogate low", "\"\\udc00\"");
    must_fail("bad escape", "\"\\q\"");

    /* Go json.Unmarshal error strings (verified against Go 1.x scanner) */
    must_fail_msg("err: empty", "", "unexpected end of JSON input");
    must_fail_msg("err: html body", "<html>oops", "invalid character '<' looking for beginning of value");
    must_fail_msg("err: literal cut off", "tru", "unexpected end of JSON input");
    must_fail_msg("err: literal wrong char", "trux", "invalid character 'x' in literal true (expecting 'e')");
    must_fail_msg("err: trailing garbage", "{} 1", "invalid character '1' after top-level value");
    must_fail_msg("err: leading zero", "01a", "invalid character '1' after top-level value");
    must_fail_msg("err: unclosed obj", "{\"a\":1", "unexpected end of JSON input");
    must_fail_msg("err: missing colon", "{\"a\" 1}", "invalid character '1' after object key");
    must_fail_msg("err: missing key quote", "{a:1}", "invalid character 'a' looking for beginning of object key string");
    must_fail_msg("err: bad separator", "{\"a\":1 \"b\":2}", "invalid character '\"' after object key:value pair");
    must_fail_msg("err: bad array sep", "[1 2]", "invalid character '2' after array element");
    must_fail_msg("err: bad escape", "\"\\q\"", "invalid character 'q' in string escape code");
    must_fail_msg("err: bad number", "-a", "invalid character 'a' in numeric literal");

    roundtrip("top-level scalar (Go accepts into interface{})",
              "12", "12");

    /* accessors */
    ne_jval *v = ne_jval_parse(
        "{\"code\":200,\"data\":[{\"url\":\"u&1\",\"n\":1.5,\"z\":null,"
        "\"ids\":[111,222]}],\"flag\":true}");
    check_int("len(data)", (long long)ne_jval_len(ne_jval_get(v, "data")), 1);
    check_int("num code", (long long)ne_jval_num(ne_jval_get(v, "code")), 200);
    check_int("num fractional",
              (long long)(ne_jval_num(
                  ne_jval_get(ne_jval_at(ne_jval_get(v, "data"), 0), "n"))
                  * 10), 15);
    check_str("str url",
              ne_jval_str(ne_jval_get(ne_jval_at(ne_jval_get(v, "data"), 0),
                                     "url")), "u&1");
    check_int("bool flag", ne_jval_bool(ne_jval_get(v, "flag")), 1);
    check_int("top-level ids absent", ne_jval_get(v, "ids") != NULL, 0);
    ne_jval *first = ne_jval_at(ne_jval_get(v, "data"), 0);
    check_int("nested ids len",
              (long long)ne_jval_len(ne_jval_get(first, "ids")), 2);
    check_str("num lexeme",
              ne_jval_num_lexeme(ne_jval_at(ne_jval_get(first, "ids"), 1)),
              "222");
    check_int("null type", ne_jval_type(ne_jval_get(first, "z")), NE_JV_NULL);
    check_int("missing key", ne_jval_get(v, "nope") != NULL, 0);
    ne_jval_free(v);

    /* builders + constructed doubles */
    ne_jval *o = ne_jval_new(NE_JV_OBJ);
    ne_jval_put(o, "code", ne_jval_new_num_d(200));
    ne_jval_put(o, "neg", ne_jval_new_num_d(-462));
    ne_jval_put(o, "s", ne_jval_new_str("a&b"));
    ne_jval *a = ne_jval_new(NE_JV_ARR);
    ne_jval_push(a, ne_jval_new_bool(0));
    ne_jval_push(a, ne_jval_clone(ne_jval_get(o, "s")));
    ne_jval_put(o, "list", a);
    ne_jval_put(o, "code", ne_jval_new_num_d(404));   /* replace */
    char *m = ne_jval_marshal(o);
    check_str("builder marshal", m,
              "{\"code\":404,\"list\":[false,\"a\\u0026b\"],"
              "\"neg\":-462,\"s\":\"a\\u0026b\"}");
    free(m);
    ne_jval_free(o);

    printf(failures ? "jval: %d FAILURES\n" : "jval: all ok\n", failures);
    return failures ? 1 : 0;
}
