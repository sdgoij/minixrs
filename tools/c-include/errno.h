/* errno for the minix OS — values match minix/include/errno.h (the
 * kernel/IPC layer returns these negated; minix-libc converts to POSIX). */
#ifndef _ERRNO_H
#define _ERRNO_H

extern int *__errno_location(void);
#define errno (*__errno_location())

#define EPERM 1
#define ENOENT 2
#define ESRCH 3
#define EINTR 4
#define EIO 5
#define ENXIO 6
#define EBADF 9
#define EAGAIN 11
#define ENOMEM 12
#define EACCES 13
#define EFAULT 14
#define EBUSY 16
#define EEXIST 17
#define ENODEV 19
#define ENOTDIR 20
#define EISDIR 21
#define EINVAL 22
#define ENOSPC 28
#define EDOM 33
#define ERANGE 34
#define ENOSYS 71
#define EOPNOTSUPP 95
#define EAFNOSUPPORT 97

#endif
