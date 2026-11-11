#include "netease/jval.h"
#include <stdio.h>
#include <string.h>
static int ref(const char *t, double *out) {
    ne_jval *root = ne_jval_parse(t);
    ne_jval *code = root && ne_jval_type(root) == NE_JV_OBJ
                  ? ne_jval_get(root, "code") : NULL;
    int ok = code && ne_jval_type(code) == NE_JV_NUM;
    if (ok) *out = ne_jval_num(code);
    ne_jval_free(root);
    return ok;
}
int main(void) {
    static char buf[300000];
    for (int n = 1; n <= 140; n++) {   /* 数组嵌套层数 */
        char *p = buf;
        p += sprintf(p, "{\"code\":200,\"x\":");
        for (int i = 0; i < n; i++) *p++ = '[';
        *p++ = '1';
        for (int i = 0; i < n; i++) *p++ = ']';
        *p++ = '}';
        *p = 0;
        double a = -9, b = -9;
        int ra = ref(buf, &a), rb = ne_jval_scan_top_code(buf, &b);
        if (ra != rb || (ra && a != b)) {
            printf("DEPTH MISMATCH n=%d: ref=%d scan=%d\n", n, ra, rb);
            return 1;
        }
    }
    printf("DEPTH SWEEP PASS (1..140 层两侧同判)\n");
    return 0;
}
