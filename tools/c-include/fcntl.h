/* Minimal fcntl.h for the minix OS — flags match minix/include/fcntl.h. */
#ifndef _FCNTL_H
#define _FCNTL_H

#define O_RDONLY 0o00
#define O_WRONLY 0o01
#define O_RDWR 0o02
#define O_CREAT 0o100
#define O_EXCL 0o200
#define O_TRUNC 0o1000
#define O_APPEND 0o2000

int open(const char *path, int flags, ...);

#endif
