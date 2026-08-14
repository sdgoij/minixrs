/* Minimal sys/statvfs.h for the minix OS. The struct layout matches what
 * the FS server writes (f_bsize/f_frsize are 32-bit), not the glibc
 * layout. */
#ifndef _SYS_STATVFS_H
#define _SYS_STATVFS_H

#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned long fsblkcnt_t;
typedef unsigned long fsfilcnt_t;

#define ST_RDONLY 0x0001
#define ST_NOSUID 0x0002
#define MNT_LOCAL 0x0004

struct statvfs {
    unsigned long f_flags;
    unsigned int f_bsize;
    unsigned int f_frsize;
    fsblkcnt_t f_blocks;
    fsblkcnt_t f_bfree;
    fsblkcnt_t f_bavail;
    fsfilcnt_t f_files;
    fsfilcnt_t f_ffree;
    fsfilcnt_t f_favail;
    unsigned long f_fsid;
    unsigned long f_flag;
    unsigned long f_namemax;
};

int statvfs(const char *path, struct statvfs *buf);
int fstatvfs(int fd, struct statvfs *buf);

#ifdef __cplusplus
}
#endif

#endif
