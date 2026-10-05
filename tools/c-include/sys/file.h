/* sys/file.h for the minix OS.
 *
 * The advisory whole-file lock API (`flock`) is not provided: the VFS has no
 * file locks, so `check_function('flock')` fails and Mesa compiles its lock
 * call sites out (they are guarded by `HAVE_FLOCK`). The `LOCK_*` constants and
 * `struct flock` are still needed because headers include this and `fcntl.h`. */
#ifndef _SYS_FILE_H
#define _SYS_FILE_H

#include <fcntl.h>

#define LOCK_SH 1
#define LOCK_EX 2
#define LOCK_NB 4
#define LOCK_UN 8

/* The VFS has no file locks, so flock() is granted unconditionally; see the
 * comment on the implementation in minix-libc. */
int flock(int fd, int operation);

#endif
