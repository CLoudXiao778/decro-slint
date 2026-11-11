/* album-family commands: purchased digital albums + single-album detail. */
#include <stdio.h>
#include <stdlib.h>
#include "netease/jval.h"
#include "netease/request.h"
#include "netease/services.h"
#include "netease/util.h"
#include "cli_shared.h"
#include "cmds.h"

/* album-purchased [limit] [offset] — dump the purchased digital-album list
 * (api/digitalAlbum/purchased). */
int cmd_album_purchased(const char *limit_in, const char *offset_in) {
    ne_resp *r = ne_album_purchased(limit_in, offset_in);
    if (r->err) {
        char msg[256];
        snprintf(msg, sizeof msg, "album purchased failed: err=%d", r->err);
        ne_resp_free(r);
        cli_die(msg);
    }
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        cli_die("bad album purchased response");
    }
    ne_jval_put(root, "code", ne_jval_new_num_d(r->code));
    ne_resp_free(r);
    cli_print_marshal(root);
    return 0;
}

int cmd_album(const char *id) {
    return cli_pass(ne_album_detail(id));
}