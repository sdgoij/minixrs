/* Minimal fcntl.h for the minix OS — flags match minix/include/fcntl.h. */
#ifndef _FCNTL_H
#define _FCNTL_H

#include <sys/types.h>

#define O_RDONLY 0o00
#define O_WRONLY 0o01
#define O_RDWR 0o02
#define O_CREAT 0o100
#define O_EXCL 0o200
#define O_TRUNC 0o1000
#define O_APPEND 0o2000
#define O_NONBLOCK 0o4000
/* MINIX has no O_CLOEXEC; define it as a bit open() ignores. */
#define O_CLOEXEC 0o200000

/* fcntl commands (minix/include/fcntl.h). */
#define F_DUPFD 0
#define F_GETFD 1
#define F_SETFD 2
#define F_GETFL 3
#define F_SETFL 4
#define F_GETLK 5
#define F_SETLK 6
#define F_SETLKW 7

#define F_RDLCK 0
#define F_WRLCK 1
#define F_UNLCK 2

#define FD_CLOEXEC 1

/* Advisory record locks — declared for ABI compatibility; minix has no
 * file locking, so F_SETLK/F_GETLK fail at runtime. */
struct flock {
    short l_type;
    short l_whence;
    off_t l_start;
    off_t l_len;
    pid_t l_pid;
};

#ifdef __cplusplus
extern "C" {
#endif

int open(const char *path, int flags, ...);
int fcntl(int fd, int cmd, ...);

#ifdef __cplusplus
}
#endif

#endif
