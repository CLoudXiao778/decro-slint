/* request.go `/\w*api/` rewrite tests — the regex consumes both slashes,
 * so "/api/x" → "/weapi/x" with exactly one slash. These lock the exact
 * Go semantics (a wrong double-slash here breaks the real server). */
#include "netease/request.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int failures = 0;

static void check(const char *url, const char *repl, const char *want) {
    char *got = ne_rewrite_api_segment(url, repl);
    if (strcmp(got, want) != 0) {
        fprintf(stderr, "FAIL rewrite(%s, %s)\n  got:  %s\n  want: %s\n",
                url, repl, got, want);
        failures++;
    } else {
        printf("ok rewrite %s → %s\n", url, want);
    }
    free(got);
}

int main(void) {
    const char *W = "/weapi/", *A = "/api/";

    /* weapi branch (CreateRequest) */
    check("https://music.163.com/api/cloudsearch/pc", W,
          "https://music.163.com/weapi/cloudsearch/pc");
    check("https://music.163.com/api/nuser/account/get", W,
          "https://music.163.com/weapi/nuser/account/get");
    check("https://music.163.com/api/song/enhance/player/url", W,
          "https://music.163.com/weapi/song/enhance/player/url");
    check("https://music.163.com/api/v3/discovery/recommend/songs", W,
          "https://music.163.com/weapi/v3/discovery/recommend/songs");
    /* idempotent: already /weapi/ */
    check("https://music.163.com/weapi/v3/song/detail", W,
          "https://music.163.com/weapi/v3/song/detail");
    check("https://music.163.com/weapi/user/playlist", W,
          "https://music.163.com/weapi/user/playlist");
    /* eapi matches \w*api too */
    check("https://music.163.com/eapi/song/enhance/player/url", W,
          "https://music.163.com/weapi/song/enhance/player/url");

    /* linuxapi inner url (weapi→api) */
    check("https://music.163.com/weapi/v3/playlist/detail", A,
          "https://music.163.com/api/v3/playlist/detail");
    /* /api/ stays /api/ */
    check("https://music.163.com/api/song/lyric", A,
          "https://music.163.com/api/song/lyric");

    /* no matches */
    check("https://music.163.com/api", W,
          "https://music.163.com/api");            /* no trailing slash */
    check("https://music.163.com/apix/y", W,
          "https://music.163.com/apix/y");          /* not ending in "api" */
    check("https://music.163.com/v1/discovery/recommend/resource", W,
          "https://music.163.com/v1/discovery/recommend/resource");
    check("https://music.163.com", W, "https://music.163.com");

    printf(failures ? "rewrite: %d FAILURES\n" : "rewrite: all ok\n",
           failures);
    return failures ? 1 : 0;
}
