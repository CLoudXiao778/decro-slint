/* write-family commands: like / subscribe / track-add / track-del. All emit
 * the {"code":%.0f,"body":%s} envelope via cli_envelope. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "netease/jmap.h"
#include "netease/jval.h"
#include "netease/request.h"
#include "netease/services.h"
#include "netease/util.h"
#include "cli_shared.h"
#include "cmds.h"

/* like <song_id> [true|false] — the shell bypasses LikeService and calls
 * CreateRequest on weapi/song/like directly with os=pc appver=2.7.1.198277 */
int cmd_like(const char *id, const char *like) {
    jmap *data = jmap_new();
    jmap_put(data, "trackId", id);
    jmap_put(data, "like", like ? like : "true");
    static const char *extras[] = {
        "os", "pc", "appver", "2.7.1.198277", NULL
    };
    char url[640];
    snprintf(url, sizeof url, "%s/weapi/song/like", ne_api_base());
    ne_resp *r = ne_create_weapi(url, data, extras);
    jmap_free(data);
    return cli_envelope(r);
}

int cmd_subscribe(const char *id, const char *t) {
    return cli_envelope(ne_playlist_subscribe(id, t ? t : "0"));
}

int cmd_track(const char *op, const char *pid, const char *sid) {
    return cli_envelope(ne_playlist_tracks(op, pid, sid));
}