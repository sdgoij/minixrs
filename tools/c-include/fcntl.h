/* Minimal fcntl.h for the minix OS.
 *
 * The O_* values match MINIX 3.3.0's sys/sys/fcntl.h (NetBSD's), which is the
 * ABI the port's kernel and VFS implement. The record-lock commands and struct
 * flock below are still the port's own values: minix-libc's fcntl does not
 * carry a struct flock argument yet, so no C consumer can reach them. */
#ifndef _FCNTL_H
#define _FCNTL_H

#include <sys/types.h>

#define O_RDONLY 0
#define O_WRONLY 01
#define O_RDWR 02
#define O_ACCMODE 03
#define O_NONBLOCK 04
#define O_APPEND 010
#define O_SYNC 0200
#define O_CREAT 01000
#define O_TRUNC 02000
#define O_EXCL 04000
#define O_NOCTTY 0100000
#define O_CLOEXEC 020000000
#define O_NOFOLLOW 0x00000100
#define O_DIRECTORY 0x00200000

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

/* The `*at` family's directory handles (`minix_std::fs` uses the same
 * values). `utimensat` is the caller libc++'s filesystem routes through. */
#define AT_FDCWD (-100)
#define AT_SYMLINK_NOFOLLOW 0x200
#define AT_REMOVEDIR 0x800

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
