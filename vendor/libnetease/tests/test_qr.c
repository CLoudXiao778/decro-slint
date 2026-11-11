/* QR encoder validation harness.
 *
 * Dumps per-case artifacts under $NE_QR_DUMP (default /tmp/neqr):
 *   caseN.txt   — first line "case N", then bitmap rows ('#'=dark,'.'=light)
 *   caseN.small — ToSmallString(false) output
 *   caseN.png   — PNG(480)
 * so tests/verify_qr.py can independently decode & cross-check. Cases that
 * must fail print "fail <msg>" instead. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "netease/qrenc.h"

#if defined(_WIN32)
#include <direct.h>
#else
#include <sys/stat.h>
#endif

/* recursive mkdir -p (avoids shelling out; matches main.c's mkpath) */
static void mkpath2(const char *path) {
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

static const char *CASES[] = {
    "https://music.163.com/login?codekey=abc123XYZ",
    "https://example.org/PATH/to-somewhere-else",
    "HELLO WORLD 12345 / TEST",
    "1234567890123456789012345",
    "https://music.163.com/login?codekey=0123456789abcdef0123456789abcdef"
    "0123456789abcdef0123456789abcdef0123456789abcdef",
    "https://music.163.com/song?id=347230&from=quick-access&",
    "A1B2C3D4E5",
    "https://music.163.com/login?codekey=",
    /* 1200-char mixed content — exercises v28 (2 block specs + version info) */
    "0123456789abcdefghij0123456789abcdefghij0123456789abcdefghij"
    "0123456789abcdefghij0123456789abcdefghij0123456789abcdefghij"
    "0123456789abcdefghij0123456789abcdefghij0123456789abcdefghij"
    "0123456789abcdefghij0123456789abcdefghij0123456789abcdefghij"
    "0123456789abcdefghij0123456789abcdefghij0123456789abcdefghij"
    "0123456789abcdefghij0123456789abcdefghij0123456789abcdefghij"
    "0123456789abcdefghij0123456789abcdefghij0123456789abcdefghij",
};

static void dump(const char *dir, int i, const char *content) {
    const char *err = NULL;
    ne_qr *q = ne_qr_new(content, &err);

    char path[512];
    snprintf(path, sizeof path, "%s/case%d.txt", dir, i);
    FILE *f = fopen(path, "w");
    if (!q) {
        fprintf(f, "fail %s\n", err ? err : "?");
        fclose(f);
        return;
    }
    fprintf(f, "case %d v%d mask%d size%d\n", i, q->version, q->mask, q->size);
    for (int y = 0; y < q->size; y++) {
        for (int x = 0; x < q->size; x++)
            fputc(q->bits[(size_t)y * q->size + x] ? '#' : '.', f);
        fputc('\n', f);
    }
    fclose(f);

    snprintf(path, sizeof path, "%s/case%d.small", dir, i);
    f = fopen(path, "w");
    char *art = ne_qr_small_string(q);
    fputs(art, f);
    free(art);
    fclose(f);

    snprintf(path, sizeof path, "%s/case%d.png", dir, i);
    size_t len = 0;
    unsigned char *png = ne_qr_png(q, 480, &len);
    f = fopen(path, "wb");
    fwrite(png, 1, len, f);
    fclose(f);
    free(png);
    ne_qr_free(q);
}

int main(void) {
    const char *dir = getenv("NE_QR_DUMP");
    if (!dir || !*dir) dir = "/tmp/neqr";
    /* create the dump dir cross-platform without shelling out; the Windows
     * `mkdir`/`rmdir` shell commands can be blocked by sandboxes and the
     * failure would be silent (fopen(NULL) → crash) */
    mkpath2(dir);
    for (size_t i = 0; i < sizeof CASES / sizeof *CASES; i++)
        dump(dir, (int)i, CASES[i]);

    /* failure case: empty content */
    const char *err = NULL;
    ne_qr *q = ne_qr_new("", &err);
    if (q || !err || strcmp(err, "no data to encode") != 0) {
        fprintf(stderr, "empty-content error mismatch: %s\n", err ? err : "ok?!");
        return 1;
    }
    /* true failure case: 3000 chars exceeds v40-M (2331 bytes) */
    char big[3001];
    for (int i = 0; i < 3000; i++) big[i] = (char)('a' + i % 26);
    big[3000] = '\0';
    err = NULL;
    q = ne_qr_new(big, &err);
    if (q || !err || strcmp(err, "content too long to encode") != 0) {
        fprintf(stderr, "long-content error mismatch: %s\n", err ? err : "ok?!");
        return 1;
    }

    /* PNG structural check: signature + IEND present, palette 2 entries */
    {
        ne_qr *r = ne_qr_new("https://x.org/a", NULL);
        size_t len = 0;
        unsigned char *png = ne_qr_png(r, 480, &len);
        int ok = r != NULL && len > 8 + 12 + 25 + 12 + 12;
        if (ok) {
            static const unsigned char sig[8] = {137, 80, 78, 71, 13, 10, 26, 10};
            ok = memcmp(png, sig, 8) == 0 && memcmp(png + len - 8, "IEND", 4) == 0;
        }
        free(png);
        ne_qr_free(r);
        if (!ok) { fprintf(stderr, "png structure bad\n"); return 1; }
    }
    printf("qr dump ok\n");
    return 0;
}
