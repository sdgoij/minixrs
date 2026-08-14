/* Minimal time.h for the minix OS — clock_gettime only. */
#ifndef _TIME_H
#define _TIME_H

#include <stddef.h>

typedef long time_t;
typedef long clockid_t;

struct timespec {
    long tv_sec;
    long tv_nsec;
};

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

#ifdef __cplusplus
extern "C" {
#endif

int clock_gettime(clockid_t clk_id, struct timespec *tp);
time_t time(time_t *tloc);
time_t mktime(struct tm *tm);
struct tm *localtime_r(const time_t *timep, struct tm *result);
struct tm *gmtime_r(const time_t *timep, struct tm *result);
size_t strftime(char *s, size_t max, const char *format, const struct tm *tm);

#ifdef __cplusplus
}
#endif

#endif
