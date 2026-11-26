/* Minimal time.h for the minix OS — clock_gettime only. */
#ifndef _TIME_H
#define _TIME_H

#include <stddef.h>

/* `time_t` is 64-bit on every target, which is what MINIX's own headers say
 * (`_BSD_TIME_T_` in `sys/arch/<arch>/include/ansi.h`). `long` coincides with that on
 * the three 64-bit shipping targets and does not on wasm32, where it is 32 bits: the
 * library writes a 64-bit `time_t`, so a 32-bit declaration here makes the second
 * half of the value land in the struct's next field (and, for a smaller struct, past
 * its end). The same holds for `tv_nsec`: `TimeSpec`/`TimeVal` in `crates/minix-libc`
 * are two 64-bit fields on every target. */
typedef __INT64_TYPE__ time_t;
typedef long clock_t;
typedef long clockid_t;

struct timespec {
    __INT64_TYPE__ tv_sec;
    __INT64_TYPE__ tv_nsec;
};

/* The size is the contract with the library, which fills this from a struct of two 64-bit
 * fields. A declaration half that size is written past its end, and one with `long` fields where
 * `long` is 32 bits reads the other half of `tv_sec` as `tv_nsec`: `clock_gettime` then looks
 * like it has one-second granularity and nothing reports an error. */
#if defined(__cplusplus) && __cplusplus >= 201103L
static_assert(sizeof(struct timespec) == 2 * sizeof(__INT64_TYPE__),
              "struct timespec is two 64-bit fields");
#elif !defined(__cplusplus) && defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
_Static_assert(sizeof(struct timespec) == 2 * sizeof(__INT64_TYPE__),
               "struct timespec is two 64-bit fields");
#endif

struct tm {
    int tm_sec;
    int tm_min;
    int tm_hour;
    int tm_mday;
    int tm_mon;
    int tm_year;
    int tm_wday;
    int tm_yday;
    int tm_isdst;
};

#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1

#define TIMER_ABSTIME 1

#ifdef __cplusplus
extern "C" {
#endif

int clock_gettime(clockid_t clk_id, struct timespec *tp);
int clock_nanosleep(clockid_t clk_id, int flags,
                    const struct timespec *req, struct timespec *rem);
int nanosleep(const struct timespec *req, struct timespec *rem);
time_t time(time_t *tloc);
time_t mktime(struct tm *tm);
struct tm *localtime(const time_t *timep);
struct tm *gmtime(const time_t *timep);
struct tm *localtime_r(const time_t *timep, struct tm *result);
struct tm *gmtime_r(const time_t *timep, struct tm *result);
size_t strftime(char *s, size_t max, const char *format, const struct tm *tm);

#ifdef __cplusplus
}
#endif

#endif
