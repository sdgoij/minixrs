/* Minimal sys/time.h for the minix OS — gettimeofday over clock_gettime. */
#ifndef _SYS_TIME_H
#define _SYS_TIME_H

#ifdef __cplusplus
extern "C" {
#endif

/* Both fields are 64-bit for the reason `time.h` gives for `time_t`: the library
 * writes `TimeVal`, which is two 64-bit fields on every target, so a 32-bit `long`
 * here would put `tv_usec` in the other half of `tv_sec`. */
struct timeval {
    __INT64_TYPE__ tv_sec;
    __INT64_TYPE__ tv_usec;
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
