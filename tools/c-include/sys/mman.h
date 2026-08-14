/* Minimal sys/mman.h for the minix OS — VM-backed mmap/munmap. */
#ifndef _SYS_MMAN_H
#define _SYS_MMAN_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define PROT_READ 0x01
#define PROT_WRITE 0x02
#define PROT_EXEC 0x04
#define PROT_NONE 0x00

#define MAP_SHARED 0x01
#define MAP_PRIVATE 0x02
#define MAP_FIXED 0x10
#define MAP_ANONYMOUS 0x20
#define MAP_FAILED ((void *)-1)

void *mmap(void *addr, size_t length, int prot, int flags, int fd, off_t offset);
int munmap(void *addr, size_t length);
int mprotect(void *addr, size_t length, int prot);
int msync(void *addr, size_t length, int flags);

#define MS_ASYNC 1
#define MS_INVALIDATE 2
#define MS_SYNC 4

#define MADV_NORMAL 0
#define MADV_RANDOM 1
#define MADV_SEQUENTIAL 2
#define MADV_WILLNEED 3
#define MADV_DONTNEED 4

int madvise(void *addr, size_t length, int advice);

/* POSIX shared memory — declared for ABI compatibility; minix has no
 * shm_open (the System V shmget/shmat interface is used instead). */
int shm_open(const char *name, int oflag, mode_t mode);
int shm_unlink(const char *name);

#ifdef __cplusplus
}
#endif

#endif
