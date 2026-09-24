/* utime.h for the minix OS — `utime()` and the struct it takes.
 *
 * POSIX puts this interface in its own header; `utimes()` (microseconds) is in
 * sys/time.h, and both are backed by the same VFS request. */
#ifndef _UTIME_H
#define _UTIME_H

#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

struct utimbuf {
    time_t actime;  /* access time */
    time_t modtime; /* modification time */
};

/* A null `times` stamps both fields with the current time. */
int utime(const char *path, const struct utimbuf *times);

#ifdef __cplusplus
}
#endif

#endif
