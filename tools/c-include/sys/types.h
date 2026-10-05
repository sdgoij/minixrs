/* Minimal sys/types.h for the minix OS. */
#ifndef _SYS_TYPES_H
#define _SYS_TYPES_H

#include <stddef.h>

typedef long ssize_t;
typedef long off_t;
typedef unsigned int mode_t;
typedef int pid_t;
typedef unsigned int uid_t;
typedef unsigned int gid_t;
typedef unsigned int dev_t;
typedef unsigned long ino_t;
typedef unsigned int nlink_t;

/* Device-number decomposition. BSD puts these in <sys/types.h> (Linux in
 * <sys/sysmacros.h>), and that is the branch libdrm's `drm.h` takes on a target
 * with no `__linux__`. The port's `dev_t` is 32-bit, so this is a 12/8 split
 * rather than glibc's; libdrm only feeds the result into a `/sys` path, which
 * the port stubs (`drmParseSubsystemType`, §6.10). */
#define major(dev) ((unsigned int)(((dev) >> 8) & 0xfffu))
#define minor(dev) ((unsigned int)((dev) & 0xffu))
#define makedev(ma, mi) ((dev_t)((((dev_t)(ma) & 0xfffu) << 8) | ((dev_t)(mi) & 0xffu)))

#endif
