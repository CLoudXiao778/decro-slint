#ifndef NE_UTIL_H
#define NE_UTIL_H
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* checked malloc — aborts on OOM (CLI tool, fail-loud is fine) */
static inline void *ne_xmalloc(size_t n) {
    void *p = malloc(n ? n : 1);
    if (!p) abort();
    return p;
}
static inline void *ne_xrealloc(void *p, size_t n) {
    void *q = realloc(p, n ? n : 1);
    if (!q) abort();
    return q;
}
static inline char *ne_xstrdup(const char *s) {
    char *d = ne_xmalloc(strlen(s) + 1);
    strcpy(d, s);
    return d;
}

/* millisecond / second unix timestamps */
int64_t ne_now_ms(void);
int64_t ne_now_unix(void);

/* sleep for ms milliseconds (Windows: Sleep; POSIX: nanosleep). no-op for
 * ms <= 0. Used by the optional request pacing / retry backoff. */
void ne_sleep_ms(int64_t ms);

/* thread-local storage keyword: GCC/Clang use __thread, MSVC __declspec(thread) */
#ifdef _MSC_VER
#define NE_THREAD_LOCAL __declspec(thread)
#else
#define NE_THREAD_LOCAL __thread
#endif

/* ── portable mutex ──────────────────────────────────────
 * Used to guard the process-global cookie jar against concurrent request
 * threads (Android/JNI and desktop can both drive libnetease from more than
 * one thread). pthread on POSIX/NDK, CRITICAL_SECTION on Windows. */
#ifdef _WIN32
#include <windows.h>
typedef CRITICAL_SECTION ne_mutex;
#define NE_MUTEX_INIT(m)   InitializeCriticalSection(&(m))
#define NE_MUTEX_LOCK(m)   EnterCriticalSection(&(m))
#define NE_MUTEX_UNLOCK(m) LeaveCriticalSection(&(m))
#define NE_MUTEX_DESTROY(m) DeleteCriticalSection(&(m))
#else
#include <pthread.h>
typedef pthread_mutex_t ne_mutex;
#define NE_MUTEX_INIT(m)   pthread_mutex_init(&(m), NULL)
#define NE_MUTEX_LOCK(m)   pthread_mutex_lock(&(m))
#define NE_MUTEX_UNLOCK(m) pthread_mutex_unlock(&(m))
#define NE_MUTEX_DESTROY(m) pthread_mutex_destroy(&(m))
#endif

/* POSIX string helpers MSVC lacks: strtok_s has the same signature as
 * strtok_r, _stricmp/_strnicmp are the case-insensitive comparisons */
#ifdef _MSC_VER
#define strtok_r strtok_s
#define strcasecmp _stricmp
#define strncasecmp _strnicmp
#endif
#endif
