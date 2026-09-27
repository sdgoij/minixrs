/* eventfd(3) for the minix OS. The object is VFS-internal (no filesystem, no
 * path): `eventfd` returns a descriptor over a 64-bit counter that `read`/
 * `write` move 8 bytes at a time, and that a `poll`/`select` can wait on.
 * crates/servers/src/vfs/eventfd.rs. */
#ifndef _SYS_EVENTFD_H
#define _SYS_EVENTFD_H

#include <stdint.h>
#include <fcntl.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef uint64_t eventfd_t;

/* Read returns 1 and decrements the counter instead of returning it whole. */
#define EFD_SEMAPHORE 1
#define EFD_CLOEXEC O_CLOEXEC
#define EFD_NONBLOCK O_NONBLOCK

int eventfd(unsigned int initval, int flags);
int eventfd_read(int fd, eventfd_t *value);
int eventfd_write(int fd, eventfd_t value);

#ifdef __cplusplus
}
#endif

#endif
