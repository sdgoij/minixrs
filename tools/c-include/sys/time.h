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
int utimes(const char *path, const struct timeval tv[2]);
/* As `utimes`, but on the link itself when `path` names a symlink
 * (`sys/sys/fcntl.h`'s AT_SYMLINK_NOFOLLOW). MINIX 3.3.0 has this in
 * `lib/libc/sys/lutimes.c`; VFS resolves it with PATH_RET_SYMLINK. */
int lutimes(const char *path, const struct timeval tv[2]);

#ifdef __cplusplus
}
#endif

#endif
