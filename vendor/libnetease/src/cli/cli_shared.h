#ifndef NE_CLI_SHARED_H
#define NE_CLI_SHARED_H
#include <stddef.h>
#include "netease/jval.h"
#include "netease/request.h"

/* Shared process/JSON helpers for the netease-cli command families.
 * Internal to the CLI shell; not part of the library API. */

void cli_die(const char *msg);            /* fprintf(stderr) + save jar + exit(1) */
void cli_output(const char *body);        /* body to stdout + trailing \n */
int  cli_pass(ne_resp *r);                /* output(r->body) + free + return 0 */
int  cli_envelope(ne_resp *r);            /* {"code":%.0f,"body":%s} envelope + free */
void cli_print_marshal(ne_jval *v);       /* marshal + print + free */

extern char cli_jval_err[192];            /* last parse error text */
int  cli_parse_root(const char *body, ne_jval **out);   /* json.Unmarshal → obj */
void cli_die_parse_account(void);         /* die() with the parse error text */

#endif