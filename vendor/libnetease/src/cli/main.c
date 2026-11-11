/* netease-cli (C) — drop-in replacement for the Go shell (same name, same
 * stdout protocol). This file is the dispatcher only; the per-family command
 * implementations + their JSON/argument handling live in cli_shared.c and the
 * cmd_*.c files under src/cli/. Unported commands print the same "unknown cmd"
 * error the Go build prints for junk input. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(_WIN32)
#include <direct.h>
#include <fcntl.h>
#include <io.h>
#include <windows.h>
#else
#include <sys/stat.h>
#endif

#include "netease/cookiejar.h"
#include "netease/encoding.h"
#include "netease/qrenc.h"
#include "netease/request.h"
#include "netease/services.h"
#include "netease/util.h"
#include "cli_shared.h"
#include "cmds.h"

/* recursive mkdir -p (os.MkdirAll) */
static void mkpath(const char *path) {
    char tmp[1024];
    snprintf(tmp, sizeof tmp, "%s", path);
    size_t l = strlen(tmp);
    if (l > 0 && tmp[l - 1] == '/') tmp[l - 1] = '\0';
    for (char *p = tmp + 1; *p; p++) {
        if (*p == '/') {
            *p = '\0';
#if defined(_WIN32)
            _mkdir(tmp);
#else
            mkdir(tmp, 0755);
#endif
            *p = '/';
        }
    }
#if defined(_WIN32)
    _mkdir(tmp);
#else
    mkdir(tmp, 0755);
#endif
}

/* main.go: ~/.cache/netune/cookies.txt
 * os.UserHomeDir(): $HOME on unix, %USERPROFILE% on Windows */
static const char *cookie_path(void) {
    static char path[1024];
    const char *home = getenv("HOME");
#if defined(_WIN32)
    if (!home || !*home) home = getenv("USERPROFILE");
#endif
    if (!home || !*home) home = ".";
    snprintf(path, sizeof path, "%s/.cache/netune/cookies.txt", home);
    char dir[1024];
    snprintf(dir, sizeof dir, "%s/.cache/netune", home);
    mkpath(dir);
    return path;
}

/* strings.Join(os.Args[from:], " ") */
static char *join_args(int argc, char **argv, int from) {
    size_t total = 1;
    for (int i = from; i < argc; i++) total += strlen(argv[i]) + 1;
    char *out = ne_xmalloc(total);
    out[0] = '\0';
    for (int i = from; i < argc; i++) {
        if (i > from) strcat(out, " ");
        strcat(out, argv[i]);
    }
    return out;
}

int main(int argc, char **argv) {
#if defined(_WIN32)
    /* Go's os.Stdout is binary mode (no \n→\r\n translation); match it so
     * the C shell stays byte-identical on Windows */
    _setmode(_fileno(stdout), _O_BINARY);
    _setmode(_fileno(stderr), _O_BINARY);
    /* Go's os.Args are UTF-8 (it decodes GetCommandLineW); C's argv arrives
     * in the system ANSI codepage (GBK on zh-CN), so any non-ASCII arg —
     * e.g. "新名字" for playlist-rename — would be mangled and break the
     * UTF-8 JSON/eapi we send upstream. Convert every arg to UTF-8 here. */
    for (int i = 1; i < argc; i++) {
        int wn = MultiByteToWideChar(CP_ACP, 0, argv[i], -1, NULL, 0);
        if (wn <= 0) continue;
        wchar_t *w = malloc((size_t)wn * sizeof(wchar_t));
        if (!w) continue;
        MultiByteToWideChar(CP_ACP, 0, argv[i], -1, w, wn);
        int un = WideCharToMultiByte(CP_UTF8, 0, w, -1, NULL, 0, NULL, NULL);
        char *u = malloc((size_t)un);
        if (u) {
            WideCharToMultiByte(CP_UTF8, 0, w, -1, u, un, NULL, NULL);
            argv[i] = u;
        }
        free(w);
    }
#endif

    if (argc < 2) {
        fprintf(stderr, "usage: netease-cli <cmd> [args...]\n");
        return 1;
    }

    ne_set_cookie_file(cookie_path());
    ne_jar_reload();

    const char *cmd = argv[1];
    int rc = 0;

    if (strcmp(cmd, "qr-render") == 0) {
        if (argc < 3) cli_die("usage: netease-cli qr-render <url>");
        const char *err = NULL;
        ne_qr *qr = ne_qr_new(argv[2], &err);
        if (!qr) {
            char msg[256];
            snprintf(msg, sizeof msg, "qr error: %s", err);
            cli_die(msg);
        }
        char *art = ne_qr_small_string(qr);
        fputs(art, stdout);
        free(art);
        ne_qr_free(qr);
    } else if (strcmp(cmd, "qr-image") == 0) {
        if (argc < 3) cli_die("usage: netease-cli qr-image <url>");
        const char *err = NULL;
        ne_qr *qr = ne_qr_new(argv[2], &err);
        if (!qr) {
            char msg[256];
            snprintf(msg, sizeof msg, "qr error: %s", err);
            cli_die(msg);
        }
        size_t len = 0;
        unsigned char *png = ne_qr_png(qr, 480, &len);
        if (!png) cli_die("qr png error: encode failed");
        char *b64 = ne_base64_encode(png, len);
        cli_output(b64);
        free(b64);
        free(png);
        ne_qr_free(qr);
    } else if (strcmp(cmd, "qr-key") == 0) {
        rc = cmd_qr_key();
    } else if (strcmp(cmd, "qr-check") == 0) {
        if (argc < 3) cli_die("usage: netease-cli qr-check <unikey>");
        rc = cmd_qr_check(argv[2]);
    } else if (strcmp(cmd, "login-status") == 0) {
        /* Go's cookiePath comes from filepath.Join → native backslashes on
         * Windows; render the same so output is byte-identical */
        const char *cf = ne_cookie_file();
#if defined(_WIN32)
        char wpath[1024];
        size_t cl = strlen(cf);
        for (size_t i = 0; i < cl && i < sizeof(wpath) - 1; i++)
            wpath[i] = (cf[i] == '/') ? '\\' : cf[i];
        wpath[cl < sizeof(wpath) - 1 ? cl : sizeof(wpath) - 1] = '\0';
        printf("{\"status\":\"check %s\"}\n", wpath);
#else
        printf("{\"status\":\"check %s\"}\n", cf);
#endif
    } else if (strcmp(cmd, "search") == 0) {
        char *s = join_args(argc, argv, 2);
        rc = cli_pass(ne_search(s, "1", "100", NULL));
        free(s);
    } else if (strcmp(cmd, "search-pl") == 0) {
        char *s = join_args(argc, argv, 2);
        rc = cli_pass(ne_search(s, "1000", "20", NULL));
        free(s);
    } else if (strcmp(cmd, "check-music") == 0) {
        if (argc < 3) cli_die("usage: netease-cli check-music <song_id>");
        rc = cmd_check_music(argv[2]);
    } else if (strcmp(cmd, "record-recent") == 0) {
        rc = cli_pass(ne_record_recent(argc > 2 ? argv[2] : "100"));
    } else if (strcmp(cmd, "recommend-resource") == 0) {
        rc = cli_pass(ne_recommend_resource());
    } else if (strcmp(cmd, "song-url") == 0) {
        if (argc < 3) cli_die("usage: netease-cli song-url <id> [level]");
        rc = cmd_song_url(argv[2], argc > 3 ? argv[3] : NULL);
    } else if (strcmp(cmd, "song-download-url") == 0) {
        if (argc < 3) cli_die("usage: netease-cli song-download-url <id> [level]");
        rc = cmd_song_download_url(argv[2], argc > 3 ? argv[3] : NULL);
    } else if (strcmp(cmd, "song-music-quality") == 0) {
        if (argc < 3) cli_die("usage: netease-cli song-music-quality <id>");
        rc = cmd_song_music_quality(argv[2]);
    } else if (strcmp(cmd, "song-purchased") == 0) {
        rc = cmd_song_purchased(argc > 2 ? argv[2] : NULL,
                                argc > 3 ? argv[3] : NULL);
    } else if (strcmp(cmd, "album-purchased") == 0) {
        rc = cmd_album_purchased(argc > 2 ? argv[2] : NULL,
                                 argc > 3 ? argv[3] : NULL);
    } else if (strcmp(cmd, "album") == 0) {
        if (argc < 3) cli_die("usage: netease-cli album <id>");
        rc = cmd_album(argv[2]);
    } else if (strcmp(cmd, "song-detail") == 0) {
        if (argc < 3) cli_die("usage: netease-cli song-detail <ids>");
        rc = cli_pass(ne_song_detail(argv[2]));
    } else if (strcmp(cmd, "playlist") == 0) {
        if (argc < 3) cli_die("usage: netease-cli playlist <id>");
        rc = cli_pass(ne_playlist_detail(argv[2], "0", NULL));
    } else if (strcmp(cmd, "playlist-cover") == 0) {
        if (argc < 3) cli_die("usage: netease-cli playlist-cover <id>");
        rc = cmd_playlist_cover(argv[2]);
    } else if (strcmp(cmd, "user-playlist") == 0) {
        if (argc < 3) cli_die("usage: netease-cli user-playlist <uid>");
        rc = cli_pass(ne_user_playlist(argv[2], NULL, NULL));
    } else if (strcmp(cmd, "liked") == 0) {
        rc = cmd_liked();
    } else if (strcmp(cmd, "liked-check") == 0) {
        if (argc < 3) cli_die("usage: netease-cli liked-check <song_id>");
        rc = cmd_liked_check(argv[2]);
    } else if (strcmp(cmd, "toplist") == 0) {
        rc = cli_pass(ne_toplist_detail());
    } else if (strcmp(cmd, "recommend-songs") == 0) {
        rc = cli_pass(ne_recommend_songs());
    } else if (strcmp(cmd, "recommend-playlists") == 0) {
        rc = cli_pass(ne_recommend_playlists("30"));
    } else if (strcmp(cmd, "lyric") == 0) {
        if (argc < 3) cli_die("usage: netease-cli lyric <song_id>");
        rc = cmd_lyric(argv[2]);
    } else if (strcmp(cmd, "playlist-tracks") == 0) {
        if (argc < 3) cli_die("usage: netease-cli playlist-tracks <id>");
        rc = cmd_playlist_tracks(argv[2]);
    } else if (strcmp(cmd, "account-name") == 0) {
        rc = cmd_account_name();
    } else if (strcmp(cmd, "account-info") == 0) {
        rc = cmd_account_info();
    } else if (strcmp(cmd, "vip-info") == 0) {
        rc = cli_pass(ne_vip_info());
    } else if (strcmp(cmd, "check-quality") == 0) {
        if (argc < 4) cli_die("usage: netease-cli check-quality <id> <level>");
        rc = cmd_check_quality(argv[2], argv[3]);
    } else if (strcmp(cmd, "playlists") == 0) {
        rc = cmd_playlists();
    } else if (strcmp(cmd, "login-email") == 0) {
        if (argc < 4) cli_die("usage: netease-cli login-email <email> <password>");
        rc = cli_pass(ne_login_email(argv[2], argv[3]));
    } else if (strcmp(cmd, "login-cellphone") == 0) {
        if (argc < 4) cli_die("usage: netease-cli login-cellphone <phone> <password>");
        rc = cli_pass(ne_login_cellphone(argv[2], argv[3]));
    } else if (strcmp(cmd, "login-refresh") == 0) {
        rc = cli_pass(ne_login_refresh());
    } else if (strcmp(cmd, "like") == 0) {
        if (argc < 3) cli_die("usage: netease-cli like <song_id> [true|false]");
        rc = cmd_like(argv[2], argc > 3 ? argv[3] : NULL);
    } else if (strcmp(cmd, "subscribe") == 0) {
        if (argc < 3) cli_die("usage: netease-cli subscribe <playlist_id> [1|0]");
        rc = cmd_subscribe(argv[2], argc > 3 ? argv[3] : NULL);
    } else if (strcmp(cmd, "track-add") == 0) {
        if (argc < 4) cli_die("usage: netease-cli track-add <playlist_id> <song_id>");
        rc = cmd_track("add", argv[2], argv[3]);
    } else if (strcmp(cmd, "track-del") == 0) {
        if (argc < 4) cli_die("usage: netease-cli track-del <playlist_id> <song_id>");
        rc = cmd_track("del", argv[2], argv[3]);
    } else if (strcmp(cmd, "playlist-create") == 0) {
        if (argc < 3) cli_die("usage: netease-cli playlist-create <name>");
        rc = cli_envelope(ne_playlist_create(argv[2], "0"));
    } else if (strcmp(cmd, "playlist-rename") == 0) {
        if (argc < 4) cli_die("usage: netease-cli playlist-rename <playlist_id> <new_name>");
        rc = cli_envelope(ne_playlist_update_name(argv[2], argv[3]));
    } else if (strcmp(cmd, "playlist-delete") == 0) {
        if (argc < 3) cli_die("usage: netease-cli playlist-delete <playlist_id>");
        rc = cli_envelope(ne_playlist_delete(argv[2]));
    } else if (strcmp(cmd, "artist") == 0) {
        if (argc < 3) cli_die("usage: netease-cli artist <id>");
        rc = cli_pass(ne_artist_detail(argv[2]));
    } else if (strcmp(cmd, "artist-songs") == 0) {
        if (argc < 3) cli_die("usage: netease-cli artist-songs <id> [order] [limit] [offset]");
        rc = cli_pass(ne_artist_songs(argv[2], argc > 5 ? argv[5] : NULL,
                                      argc > 4 ? argv[4] : NULL,
                                      argc > 3 ? argv[3] : NULL));
    } else if (strcmp(cmd, "artist-albums") == 0) {
        if (argc < 3) cli_die("usage: netease-cli artist-albums <id> [limit] [offset]");
        rc = cli_pass(ne_artist_albums(argv[2], argc > 3 ? argv[3] : NULL,
                                       argc > 4 ? argv[4] : NULL));
    } else if (strcmp(cmd, "artist-desc") == 0) {
        if (argc < 3) cli_die("usage: netease-cli artist-desc <id>");
        rc = cli_pass(ne_artist_desc(argv[2]));
    } else if (strcmp(cmd, "radio") == 0) {
        if (argc < 3) cli_die("usage: netease-cli radio <id>");
        rc = cli_pass(ne_radio_detail(argv[2]));
    } else if (strcmp(cmd, "radio-programs") == 0) {
        if (argc < 3) cli_die("usage: netease-cli radio-programs <radio_id> [limit] [offset] [asc]");
        rc = cli_pass(ne_radio_programs(argv[2], argc > 3 ? argv[3] : NULL,
                                        argc > 4 ? argv[4] : NULL,
                                        argc > 5 ? argv[5] : NULL));
    } else if (strcmp(cmd, "comments") == 0) {
        if (argc < 3) cli_die("usage: netease-cli comments <thread_id> [limit] [offset] [before]");
        rc = cli_pass(ne_comments(argv[2], argc > 3 ? argv[3] : NULL,
                                  argc > 4 ? argv[4] : NULL,
                                  argc > 5 ? argv[5] : NULL));
    } else if (strcmp(cmd, "comments-hot") == 0) {
        if (argc < 3) cli_die("usage: netease-cli comments-hot <thread_id> [limit] [offset] [before]");
        rc = cli_pass(ne_comments_hot(argv[2], argc > 3 ? argv[3] : NULL,
                                      argc > 4 ? argv[4] : NULL,
                                      argc > 5 ? argv[5] : NULL));
    } else {
        fprintf(stderr, "unknown cmd: %s\n", cmd);
        rc = 1;
    }

    /* persist jar (filterJar already dropped the fake NMTID in-memory too) */
    ne_jar_save_file(ne_global_jar(), ne_cookie_file());
    return rc;
}