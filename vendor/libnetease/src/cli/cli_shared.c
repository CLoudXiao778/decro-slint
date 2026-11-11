#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "netease/cookiejar.h"
#include "netease/jval.h"
#include "netease/request.h"
#include "cli_shared.h"

char cli_jval_err[192];

void cli_die(const char *msg) {
    fprintf(stderr, "%s\n", msg);
    ne_jar_save_file(ne_global_jar(), ne_cookie_file());
    exit(1);
}

void cli_output(const char *body) {
    fputs(body ? body : "", stdout);
    fputc('\n', stdout);
}

int cli_pass(ne_resp *r) {
    cli_output(r->body);
    ne_resp_free(r);
    return 0;
}

/* output for CreateRequest-style (code, body) pairs — body spliced RAW,
 * byte-identical to Go's fmt.Sprintf envelope (never re-marshalled) */
int cli_envelope(ne_resp *r) {
    printf("{\"code\":%.0f,\"body\":%s}\n", r->code, r->body ? r->body : "");
    ne_resp_free(r);
    return 0;
}

void cli_print_marshal(ne_jval *v) {
    char *b = ne_jval_marshal(v);
    printf("%s\n", b);
    free(b);
    ne_jval_free(v);
}

static const char *unmarshal_type_name(ne_jv_type t) {
    switch (t) {
        case NE_JV_STR:  return "string";
        case NE_JV_NUM:  return "number";
        case NE_JV_BOOL: return "bool";
        case NE_JV_ARR:  return "array";
        default:         return "value";
    }
}

/* json.Unmarshal into map[string]interface{} — 1 iff body is a JSON object.
 * On failure fills cli_jval_err with the Go error text (%v of Unmarshal /
 * UnmarshalTypeError) for main.go's "parse account failed: %v". */
int cli_parse_root(const char *body, ne_jval **out) {
    cli_jval_err[0] = '\0';
    *out = ne_jval_parse(body ? body : "");
    if (!*out) {
        snprintf(cli_jval_err, sizeof cli_jval_err, "%s", ne_jval_last_error());
        return 0;
    }
    if (ne_jval_type(*out) != NE_JV_OBJ) {
        /* UnmarshalTypeError: body IS valid JSON, just the wrong shape */
        snprintf(cli_jval_err, sizeof cli_jval_err,
                 "json: cannot unmarshal %s into Go value of type map[string]interface {}",
                 unmarshal_type_name(ne_jval_type(*out)));
        ne_jval_free(*out);
        *out = NULL;
        return 0;
    }
    return 1;
}

/* cli_die(fmt.Sprintf("parse account failed: %v", err)) */
void cli_die_parse_account(void) {
    char msg[256];
    snprintf(msg, sizeof msg, "parse account failed: %s",
             *cli_jval_err ? cli_jval_err : "unexpected end of JSON input");
    cli_die(msg);
}