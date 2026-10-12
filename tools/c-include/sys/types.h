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
 * with no `__linux__`. The port's `dev_t` is a 32-bit `(major << 16) | minor`: VFS
 * routes a device by `dev >> 16` (`crates/servers/src/vfs/device.rs`) and the
 * image builder writes device nodes with the same split
 * (`crates/boot-image/src/minixfs.rs`), so the `st_rdev` a C program reads with
 * `stat` is in that form. libdrm's OpenBSD/DragonFly device discovery feeds
 * `st_rdev` through these macros to name the render node's driver (§6.10), and a
 * mismatch (`major()` returning 256 for major 145) makes it drop the node
 * entirely. The minor is 16 bits because a socket's clone minor is allocated in
 * the low 16 bits. */
#define major(dev) ((unsigned int)((unsigned int)(dev) >> 16))
#define minor(dev) ((unsigned int)((unsigned int)(dev) & 0xffffu))
#define makedev(ma, mi) ((dev_t)((((dev_t)(ma) & 0xffffu) << 16) | ((dev_t)(mi) & 0xffffu)))

#endif
