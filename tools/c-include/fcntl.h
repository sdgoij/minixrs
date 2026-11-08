/* Minimal fcntl.h for the minix OS.
 *
 * The O_* values below predate the port's C clients and are not all the values in
 * minix/include/fcntl.h; O_CREAT is 0x40 there rather than 0x200, and O_CLOEXEC is
 * 0x10000 rather than 0x400000. They are written here in C octal, which is what the
 * clients have always passed. */
#ifndef _FCNTL_H
#define _FCNTL_H

#include <sys/types.h>

#define O_RDONLY 0
#define O_WRONLY 01
#define O_RDWR 02
#define O_CREAT 0100
#define O_EXCL 0200
#define O_TRUNC 01000
#define O_APPEND 02000
#define O_NONBLOCK 04000
/* MINIX has no O_CLOEXEC; define it as a bit open() ignores. */
#define O_CLOEXEC 0200000

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
int openat(int dirfd, const char *path, int flags, ...);
int fcntl(int fd, int cmd, ...);
int posix_fadvise(int fd, off_t offset, off_t len, int advice);

#ifdef __cplusplus
}
#endif

#endif
