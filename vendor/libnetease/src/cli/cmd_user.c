/* user-family commands: liked / liked-check / playlists / playlist-cover /
 * lyric / playlist-tracks / account-name / account-info. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "netease/jval.h"
#include "netease/request.h"
#include "netease/services.h"
#include "netease/util.h"
#include "cli_shared.h"
#include "cmds.h"

/* account.id from UserAccountService body (liked / liked-check / playlists);
 * returns 1 with *uid set, 0 on unparseable body (caller dies) */
static int account_uid(long long *uid) {
    ne_resp *r = ne_user_account();
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        return 0;
    }
    long long uid_v = 0;
    ne_jval *acc = ne_jval_get(root, "account");
    if (acc && ne_jval_type(acc) == NE_JV_OBJ) {
        ne_jval *idv = ne_jval_get(acc, "id");
        if (idv && ne_jval_type(idv) == NE_JV_NUM)
            uid_v = (long long)ne_jval_num(idv);
    }
    ne_jval_free(root);
    ne_resp_free(r);
    *uid = uid_v;
    return 1;
}

/* liked / liked-check / playlists share account+likelist scaffolding */
static ne_jval *like_ids_of(long long uid, ne_resp **resp_out) {
    char uid_str[32];
    snprintf(uid_str, sizeof uid_str, "%lld", uid);
    ne_resp *r = ne_like_list(uid_str);
    ne_jval *root = NULL;
    cli_parse_root(r->body, &root);   /* Go ignores the error here (liked-check) */
    ne_jval *ids = root ? ne_jval_get(root, "ids") : NULL;
    if (!ids || ne_jval_type(ids) != NE_JV_ARR) {
        ne_jval_free(root);
        ne_resp_free(r);
        *resp_out = NULL;
        return NULL;
    }
    /* detach the array from the tree, free the rest */
    ne_jval *ids_copy = ne_jval_clone(ids);
    ne_jval_free(root);
    if (resp_out) *resp_out = r; else ne_resp_free(r);
    return ids_copy;
}

int cmd_liked(void) {
    long long uid = 0;
    if (!account_uid(&uid)) cli_die_parse_account();
    if (uid == 0) cli_die("failed to get uid, need login first");

    ne_jval *ids = like_ids_of(uid, NULL);
    if (!ids) {
        /* Go: parse failure → "parse liked failed: %v"; valid JSON but
         * ids missing/empty → "no liked songs or parse failed" */
        if (*cli_jval_err) {
            char msg[256];
            snprintf(msg, sizeof msg, "parse liked failed: %s", cli_jval_err);
            cli_die(msg);
        }
        cli_die("no liked songs or parse failed");
    }
    if (ne_jval_len(ids) == 0) cli_die("no liked songs or parse failed");

    /* "%.0f" per element, then 200-id batches of SongDetail */
    size_t total = ne_jval_len(ids);
    char **id_strs = ne_xmalloc((total ? total : 1) * sizeof(char *));
    size_t n = 0;
    for (size_t i = 0; i < total; i++) {
        ne_jval *e = ne_jval_at(ids, i);
        if (e && ne_jval_type(e) == NE_JV_NUM) {
            char buf[32];
            snprintf(buf, sizeof buf, "%.0f", ne_jval_num(e));
            id_strs[n++] = ne_xstrdup(buf);
        }
    }
    ne_jval_free(ids);
    if (n == 0) cli_die("no liked songs");

    ne_jval *all = ne_jval_new(NE_JV_ARR);
    for (size_t i = 0; i < n; i += 200) {
        size_t end = i + 200 < n ? i + 200 : n;
        size_t csv_len = 1;
        for (size_t k = i; k < end; k++) csv_len += strlen(id_strs[k]) + 1;
        char *csv = ne_xmalloc(csv_len);
        csv[0] = '\0';
        for (size_t k = i; k < end; k++) {
            if (k > i) strcat(csv, ",");
            strcat(csv, id_strs[k]);
        }
        ne_resp *dr = ne_song_detail(csv);
        free(csv);
        ne_jval *droot = NULL;
        if (cli_parse_root(dr->body, &droot)) {
            ne_jval *songs = ne_jval_get(droot, "songs");
            if (songs && ne_jval_type(songs) == NE_JV_ARR)
                for (size_t k = 0; k < ne_jval_len(songs); k++)
                    ne_jval_push(all, ne_jval_clone(ne_jval_at(songs, k)));
        }
        ne_jval_free(droot);
        ne_resp_free(dr);
    }
    for (size_t i = 0; i < n; i++) free(id_strs[i]);
    free(id_strs);

    if (ne_jval_len(all) > 0) {
        ne_jval *out = ne_jval_new(NE_JV_OBJ);
        ne_jval_put(out, "code", ne_jval_new_num_d(200));
        ne_jval *res = ne_jval_new(NE_JV_OBJ);
        ne_jval_put(res, "songs", all);
        ne_jval_put(out, "result", res);
        cli_print_marshal(out);
    } else {
        ne_jval_free(all);
        printf("\n");   /* Go: fmt.Println() on empty */
    }
    return 0;
}

int cmd_liked_check(const char *song_id) {
    long long uid = 0;
    if (!account_uid(&uid)) cli_die_parse_account();
    if (uid == 0) cli_die("failed to get uid, need login first");

    int liked = 0;
    ne_jval *ids = like_ids_of(uid, NULL);
    if (ids) {
        for (size_t i = 0; i < ne_jval_len(ids); i++) {
            ne_jval *e = ne_jval_at(ids, i);
            if (e && ne_jval_type(e) == NE_JV_NUM) {
                char buf[32];
                snprintf(buf, sizeof buf, "%.0f", ne_jval_num(e));
                if (strcmp(buf, song_id) == 0) {
                    liked = 1;
                    break;
                }
            }
        }
        ne_jval_free(ids);
    }
    printf("{\"code\":200,\"liked\":%s}\n", liked ? "true" : "false");
    return 0;
}

int cmd_playlists(void) {
    long long uid = 0;
    if (!account_uid(&uid)) cli_die_parse_account();
    if (uid == 0) cli_die("failed to get uid, need login first");

    char uid_str[32];
    snprintf(uid_str, sizeof uid_str, "%lld", uid);

    ne_jval *all = ne_jval_new(NE_JV_ARR);
    int offset = 0;
    for (;;) {
        char limit_str[16], offset_str[16];
        snprintf(limit_str, sizeof limit_str, "100");
        snprintf(offset_str, sizeof offset_str, "%d", offset);
        ne_resp *r = ne_user_playlist(uid_str, limit_str, offset_str);
        ne_jval *root = NULL;
        if (!cli_parse_root(r->body, &root)) {
            cli_output(r->body);       /* Go prints the raw body and returns */
            ne_resp_free(r);
            ne_jval_free(all);
            return 0;
        }
        ne_jval *pl = ne_jval_get(root, "playlist");
        if (!pl || ne_jval_type(pl) != NE_JV_ARR || ne_jval_len(pl) == 0) {
            ne_jval_free(root);
            ne_resp_free(r);
            break;
        }
        for (size_t i = 0; i < ne_jval_len(pl); i++) {
            ne_jval *pm = ne_jval_at(pl, i);
            if (!pm || ne_jval_type(pm) != NE_JV_OBJ) continue;
            long long pid = 0;
            const char *name = NULL;
            int subscribed = 0;
            ne_jval *idv = ne_jval_get(pm, "id");
            if (idv && ne_jval_type(idv) == NE_JV_NUM)
                pid = (long long)ne_jval_num(idv);
            ne_jval *nv = ne_jval_get(pm, "name");
            if (nv && ne_jval_type(nv) == NE_JV_STR)
                name = ne_jval_str(nv);
            ne_jval *sv = ne_jval_get(pm, "subscribed");
            if (sv && ne_jval_type(sv) == NE_JV_BOOL)
                subscribed = ne_jval_bool(sv);
            ne_jval *cov = ne_jval_get(pm, "coverImgUrl");
            const char *cover =
                (cov && ne_jval_type(cov) == NE_JV_STR) ? ne_jval_str(cov) : "";
            if (pid > 0) {
                ne_jval *o = ne_jval_new(NE_JV_OBJ);
                char idbuf[32];
                snprintf(idbuf, sizeof idbuf, "%lld", pid);
                ne_jval_put(o, "id", ne_jval_new_num(idbuf));
                ne_jval_put(o, "name", ne_jval_new_str(name ? name : ""));
                ne_jval_put(o, "subscribed", ne_jval_new_bool(subscribed));
                ne_jval_put(o, "coverImgUrl", ne_jval_new_str(cover));
                ne_jval_push(all, o);
            }
        }
        size_t got = ne_jval_len(pl);
        ne_jval_free(root);
        ne_resp_free(r);
        if ((int)got < 100) break;
        offset += 100;
    }

    ne_jval *out = ne_jval_new(NE_JV_OBJ);
    ne_jval_put(out, "code", ne_jval_new_num_d(200));
    ne_jval_put(out, "playlists", all);
    cli_print_marshal(out);
    return 0;
}

int cmd_playlist_cover(const char *id) {
    ne_resp *r = ne_playlist_detail(id, "0", NULL);
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        cli_output(r->body);
        ne_resp_free(r);
        return 0;
    }
    ne_jval *pl = ne_jval_get(root, "playlist");
    const char *cover = "";
    if (pl && ne_jval_type(pl) == NE_JV_OBJ) {
        ne_jval *cov = ne_jval_get(pl, "coverImgUrl");
        if (cov && ne_jval_type(cov) == NE_JV_STR && ne_jval_str(cov))
            cover = ne_jval_str(cov);
    }
    ne_jval *out = ne_jval_new(NE_JV_OBJ);
    ne_jval_put(out, "code", ne_jval_new_num_d(200));
    ne_jval_put(out, "coverImgUrl", ne_jval_new_str(cover));
    ne_jval_free(root);
    ne_resp_free(r);
    cli_print_marshal(out);
    return 0;
}

int cmd_lyric(const char *id) {
    ne_resp *r = ne_lyric(id);
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        cli_output(r->body);
        ne_resp_free(r);
        return 0;
    }

    /* prefer tlyric (translated), fallback to lrc; klyric when present */
    const char *lyric_text = "", *klyric_text = "";
    ne_jval *t = ne_jval_get(root, "tlyric");
    if (t && ne_jval_type(t) == NE_JV_OBJ) {
        ne_jval *l = ne_jval_get(t, "lyric");
        if (l && ne_jval_type(l) == NE_JV_STR) lyric_text = ne_jval_str(l);
    }
    if (!*lyric_text) {
        ne_jval *lrc = ne_jval_get(root, "lrc");
        if (lrc && ne_jval_type(lrc) == NE_JV_OBJ) {
            ne_jval *l = ne_jval_get(lrc, "lyric");
            if (l && ne_jval_type(l) == NE_JV_STR) lyric_text = ne_jval_str(l);
        }
    }
    ne_jval *kl = ne_jval_get(root, "klyric");
    if (kl && ne_jval_type(kl) == NE_JV_OBJ) {
        ne_jval *l = ne_jval_get(kl, "lyric");
        if (l && ne_jval_type(l) == NE_JV_STR) klyric_text = ne_jval_str(l);
    }

    ne_jval *out = ne_jval_new(NE_JV_OBJ);
    ne_jval *code = ne_jval_get(root, "code");
    if (code && ne_jval_type(code) == NE_JV_NUM)
        ne_jval_put(out, "code", ne_jval_new_num(ne_jval_num_lexeme(code)));
    if (*lyric_text)
        ne_jval_put(out, "lyric", ne_jval_new_str(lyric_text));
    if (*klyric_text)
        ne_jval_put(out, "klyric", ne_jval_new_str(klyric_text));
    ne_jval_free(root);
    ne_resp_free(r);
    cli_print_marshal(out);
    return 0;
}

int cmd_playlist_tracks(const char *id) {
    ne_resp *r = ne_playlist_detail(id, "0", NULL);
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        cli_output(r->body);
        ne_resp_free(r);
        return 0;
    }
    ne_jval *pl = ne_jval_get(root, "playlist");
    if (!pl || ne_jval_type(pl) != NE_JV_OBJ) {
        cli_output(r->body);
        ne_jval_free(root);
        ne_resp_free(r);
        return 0;
    }
    ne_jval *tracks = ne_jval_get(pl, "tracks");
    if (!tracks || ne_jval_type(tracks) != NE_JV_ARR) {
        cli_output(r->body);
        ne_jval_free(root);
        ne_resp_free(r);
        return 0;
    }

    ne_jval *out = ne_jval_new(NE_JV_OBJ);
    ne_jval_put(out, "code", ne_jval_new_num_d(200));
    ne_jval *res = ne_jval_new(NE_JV_OBJ);
    ne_jval_put(res, "songs", ne_jval_clone(tracks));
    ne_jval_put(out, "result", res);
    ne_jval_free(root);
    ne_resp_free(r);
    cli_print_marshal(out);
    return 0;
}

int cmd_account_name(void) {
    ne_resp *r = ne_user_account();
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        puts("error");
        return 0;
    }
    const char *name = NULL;
    ne_jval *prof = ne_jval_get(root, "profile");
    if (prof && ne_jval_type(prof) == NE_JV_OBJ) {
        ne_jval *n = ne_jval_get(prof, "nickname");
        if (n && ne_jval_type(n) == NE_JV_STR) name = ne_jval_str(n);
    }
    if (!name || !*name) {
        ne_jval *acc = ne_jval_get(root, "account");
        if (acc && ne_jval_type(acc) == NE_JV_OBJ) {
            ne_jval *n = ne_jval_get(acc, "userName");
            if (n && ne_jval_type(n) == NE_JV_STR) name = ne_jval_str(n);
        }
    }
    puts(name && *name ? name : "未登录");
    ne_jval_free(root);
    ne_resp_free(r);
    return 0;
}

/* account-info — account-level entitlement: vipType + login state + name */
int cmd_account_info(void) {
    ne_resp *r = ne_user_account();
    ne_jval *root = NULL;
    if (!cli_parse_root(r->body, &root)) {
        ne_resp_free(r);
        ne_jval *out = ne_jval_new(NE_JV_OBJ);
        ne_jval_put(out, "vipType", ne_jval_new_num_d(0));
        ne_jval_put(out, "name", ne_jval_new_str(""));
        ne_jval_put(out, "login", ne_jval_new_bool(0));
        cli_print_marshal(out);
        return 0;
    }
    ne_jval *prof = ne_jval_get(root, "profile");
    int login = prof && ne_jval_type(prof) == NE_JV_OBJ;
    long long vip = 0;
    const char *name = NULL;
    if (login) {
        ne_jval *vt = ne_jval_get(prof, "vipType");
        if (vt && ne_jval_type(vt) == NE_JV_NUM)
            vip = (long long)ne_jval_num(vt);
        ne_jval *n = ne_jval_get(prof, "nickname");
        if (n && ne_jval_type(n) == NE_JV_STR) name = ne_jval_str(n);
    }
    if (!name || !*name) {
        ne_jval *acc = ne_jval_get(root, "account");
        if (acc && ne_jval_type(acc) == NE_JV_OBJ) {
            ne_jval *n = ne_jval_get(acc, "userName");
            if (n && ne_jval_type(n) == NE_JV_STR) name = ne_jval_str(n);
        }
    }
    ne_jval *out = ne_jval_new(NE_JV_OBJ);
    ne_jval_put(out, "vipType", ne_jval_new_num_d((double)vip));
    ne_jval_put(out, "name", ne_jval_new_str(name && *name ? name : ""));
    ne_jval_put(out, "login", ne_jval_new_bool(login));
    ne_jval_free(root);
    ne_resp_free(r);
    cli_print_marshal(out);
    return 0;
}