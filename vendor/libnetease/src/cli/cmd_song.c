/* song-family commands: check-music, song-url, song-download-url,
 * song-music-quality, song-purchased, check-quality. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "netease/jval.h"
#include "netease/request.h"
#include "netease/services.h"
#include "netease/util.h"
#include "cli_shared.h"
#include "cmds.h"

/* level whitelist — mirrors service.SongQualityLevel.IsValid (v1.6.0):
 * standard/higher/exhigh/lossless/hires/jyeffect/sky/jymaster */
static int ne_level_valid(const char *level) {
    static const char *const kLevels[] = {
        "standard", "higher", "exhigh", "lossless", "hires",
        "jyeffect", "sky", "jymaster", NULL
    };
    if (!level || !*level) return 0;
    for (int i = 0; kLevels[i]; i++)
        if (strcmp(level, kLevels[i]) == 0) return 1;
    return 0;
}

int cmd_check_music(const char *id) {
    ne_resp *r = ne_check_music(id, NULL);
    int playable = 0;
    ne_jval *root = NULL;
    if (cli_parse_root(r->body, &root)) {
        ne_jval *code = ne_jval_get(root, "code");
        ne_jval *data = ne_jval_get(root, "data");
        if (code && ne_jval_type(code) == NE_JV_NUM &&
            (long long)ne_jval_num(code) == 200 &&
            data && ne_jval_type(data) == NE_JV_ARR &&
            ne_jval_len(data) > 0) {
            ne_jval *url = ne_jval_get(ne_jval_at(data, 0), "url");
            if (url && ne_jval_type(url) == NE_JV_STR &&
                ne_jval_str(url)[0])
                playable = 1;
        }
    }
    ne_jval_free(root);
    ne_resp_free(r);
    /* Go: fmt.Sprintf — literal text, not json.Marshal */
    printf("{\"code\":200,\"playable\":%s}\n", playable ? "true" : "false");
    return 0;
}

int cmd_song_url(const char *id, const char *level_in) {
    if (level_in && !ne_level_valid(level_in)) {
        char msg[256];
        snprintf(msg, sizeof msg, "bad level: %s", level_in);
        cli_die(msg);
    }
    const char *level = level_in ? level_in : "standard";

    /* V1 first; restricted → fall back to the old linuxapi endpoint */
    ne_resp *v1 = ne_song_url_v1(id, level);
    int have_v1 = 0;
    ne_jval *root = NULL;
    if (v1->err == 0 && cli_parse_root(v1->body, &root)) {
        ne_jval *items = ne_jval_get(root, "data");
        if (items && ne_jval_type(items) == NE_JV_ARR &&
            ne_jval_len(items) > 0) {
            ne_jval *item = ne_jval_at(items, 0);
            ne_jval *u = ne_jval_get(item, "url");
            if (u && ne_jval_type(u) == NE_JV_STR && ne_jval_str(u)[0])
                have_v1 = 1;
            /* item-level non-200 code or freeTrialInfo (non-null) forces
             * the fallback (main.go song-url) */
            ne_jval *ic = ne_jval_get(item, "code");
            int cleared = 0;
            if (ic && ne_jval_type(ic) == NE_JV_NUM &&
                ne_jval_num(ic) != 0 &&
                (long long)ne_jval_num(ic) != 200) {
                have_v1 = 0;
                cleared = 1;
            }
            if (!cleared) {
                ne_jval *ft = ne_jval_get(item, "freeTrialInfo");
                if (ft && ne_jval_type(ft) != NE_JV_NULL)
                    have_v1 = 0;
            }
        }
    }
    ne_jval_free(root);

    double final_code;
    char *final_body;
    if (have_v1) {
        final_code = v1->code;
        final_body = ne_xstrdup(v1->body);
        ne_resp_free(v1);
    } else {
        ne_resp_free(v1);
        const char *br = "320000";
        if (strcmp(level, "lossless") == 0 || strcmp(level, "hires") == 0)
            br = "999000";
        ne_resp *old = ne_song_url_old(id, br);
        final_code = old->code;
        final_body = ne_xstrdup(old->body);
        ne_resp_free(old);
    }

    /* re-marshal with the final code (map semantics: sorted keys) */
    ne_jval *out = NULL;
    if (!cli_parse_root(final_body, &out)) {
        ne_jval_free(out);
        out = ne_jval_new(NE_JV_OBJ);
    }
    ne_jval_put(out, "code", ne_jval_new_num_d(final_code));
    cli_print_marshal(out);
    free(final_body);
    return 0;
}

/* song-download-url <id> [level] — official download endpoint (original
 * apiservice). data comes back as a single object; wrap it in an array so
 * the C consumer parses it like the play-URL response. */
int cmd_song_download_url(const char *id, const char *level_in) {
    if (level_in && !ne_level_valid(level_in)) {
        char msg[256];
        snprintf(msg, sizeof msg, "bad level: %s", level_in);
        cli_die(msg);
    }
    const char *level = level_in ? level_in : "standard";
    ne_resp *r = ne_song_download_url(id, level);
    if (r->err) {
        char msg[256];
        snprintf(msg, sizeof msg, "download url failed: err=%d", r->err);
        ne_resp_free(r);
        cli_die(msg);
    }
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        cli_die("bad download url response");
    }
    ne_jval *d = ne_jval_get(root, "data");
    if (d && ne_jval_type(d) == NE_JV_OBJ) {
        ne_jval *arr = ne_jval_new(NE_JV_ARR);
        ne_jval_push(arr, ne_jval_clone(d));
        ne_jval_put(root, "data", arr);
    }
    ne_jval_put(root, "code", ne_jval_new_num_d(r->code));
    ne_resp_free(r);
    cli_print_marshal(root);
    return 0;
}

/* song-music-quality <id> — dump the track's per-tier source table
 * (l/m/h/sq/hr/je/sk/jm → {br,size,...}) straight from the response. The
 * caller uses this to decide which levels a track actually has. */
int cmd_song_music_quality(const char *id) {
    ne_resp *r = ne_song_music_quality(id);
    if (r->err) {
        char msg[256];
        snprintf(msg, sizeof msg, "song music quality failed: err=%d", r->err);
        ne_resp_free(r);
        cli_die(msg);
    }
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        cli_die("bad song music quality response");
    }
    ne_jval_put(root, "code", ne_jval_new_num_d(r->code));
    ne_resp_free(r);
    cli_print_marshal(root);
    return 0;
}

/* song-purchased [limit] [offset] — dump the purchased single-track list
 * (api/single/mybought/song/list). The list itself is owned content. */
int cmd_song_purchased(const char *limit_in, const char *offset_in) {
    ne_resp *r = ne_song_purchased(limit_in, offset_in);
    if (r->err) {
        char msg[256];
        snprintf(msg, sizeof msg, "song purchased failed: err=%d", r->err);
        ne_resp_free(r);
        cli_die(msg);
    }
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        cli_die("bad song purchased response");
    }
    ne_jval_put(root, "code", ne_jval_new_num_d(r->code));
    ne_resp_free(r);
    cli_print_marshal(root);
    return 0;
}

/* check-quality <id> <level> — single-level entitlement probe. Requests
 * EXACTLY the given level from player/url/v1 (no fallback, no quality
 * ladder) and reports the server's verdict verbatim: granted / free_trial /
 * denied / no_url / no_data, plus the bitrate/level actually granted. */
int cmd_check_quality(const char *id, const char *level) {
    if (!ne_level_valid(level)) {
        char msg[256];
        snprintf(msg, sizeof msg, "bad level: %s", level);
        cli_die(msg);
    }
    ne_resp *r = ne_song_url_v1(id, level);
    if (r->err) {
        char msg[256];
        snprintf(msg, sizeof msg, "quality check failed: err=%d", r->err);
        ne_resp_free(r);
        cli_die(msg);
    }
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        cli_die("bad quality check response");
    }
    int granted = 0;
    const char *reason = "no_data";
    double item_code = 0;
    int has_code = 0;
    double br = 0;
    const char *glevel = "";
    ne_jval *items = ne_jval_get(root, "data");
    ne_jval *item = NULL;
    if (items && ne_jval_type(items) == NE_JV_ARR && ne_jval_len(items) > 0)
        item = ne_jval_at(items, 0);
    if (item) {
        ne_jval *ic = ne_jval_get(item, "code");
        if (ic && ne_jval_type(ic) == NE_JV_NUM) {
            item_code = ne_jval_num(ic);
            has_code = 1;
        }
        ne_jval *u = ne_jval_get(item, "url");
        const char *url = (u && ne_jval_type(u) == NE_JV_STR)
                          ? ne_jval_str(u) : "";
        ne_jval *ft = ne_jval_get(item, "freeTrialInfo");
        int trial = ft && ne_jval_type(ft) != NE_JV_NULL;
        ne_jval *b = ne_jval_get(item, "br");
        if (b && ne_jval_type(b) == NE_JV_NUM) br = ne_jval_num(b);
        ne_jval *lv = ne_jval_get(item, "level");
        if (lv && ne_jval_type(lv) == NE_JV_STR) glevel = ne_jval_str(lv);
        if (has_code && item_code != 0 && item_code != 200) {
            reason = "denied";
        } else if (trial) {
            reason = "free_trial";
        } else if (!url[0]) {
            reason = "no_url";
        } else {
            granted = 1;
            reason = "ok";
        }
    }
    ne_jval *out = ne_jval_new(NE_JV_OBJ);
    ne_jval_put(out, "requested", ne_jval_new_str(level));
    ne_jval_put(out, "granted", ne_jval_new_bool(granted));
    ne_jval_put(out, "reason", ne_jval_new_str(reason));
    ne_jval_put(out, "code", ne_jval_new_num_d(has_code ? item_code : r->code));
    ne_jval_put(out, "granted_br", ne_jval_new_num_d(br));
    ne_jval_put(out, "granted_level", ne_jval_new_str(glevel));
    ne_jval_free(root);
    ne_resp_free(r);
    cli_print_marshal(out);
    return 0;
}