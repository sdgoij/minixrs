/* Minimal sys/time.h for the minix OS — gettimeofday over clock_gettime. */
#ifndef _SYS_TIME_H
#define _SYS_TIME_H

#ifdef __cplusplus
extern "C" {
#endif

struct timeval {
    long tv_sec;
    long tv_usec;
};

int gettimeofday(struct timeval *tv, void *tz);

#ifdef __cplusplus
}
#endif

#endif
