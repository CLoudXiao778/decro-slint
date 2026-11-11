#include "netease/util.h"
#include <time.h>
#ifdef _WIN32
#include <windows.h>
#endif

int64_t ne_now_ms(void) {
    struct timespec ts;
#if defined(_WIN32)
    /* WallClock as ms since epoch */
    return (int64_t)time(NULL) * 1000;
#else
    clock_gettime(CLOCK_REALTIME, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
#endif
}
int64_t ne_now_unix(void) { return (int64_t)time(NULL); }

void ne_sleep_ms(int64_t ms) {
    if (ms <= 0) return;
#ifdef _WIN32
    Sleep((DWORD)ms);
#else
    struct timespec ts;
    ts.tv_sec = ms / 1000;
    ts.tv_nsec = (long)(ms % 1000) * 1000000L;
    nanosleep(&ts, NULL);
#endif
}
