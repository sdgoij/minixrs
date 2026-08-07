/* Minimal unistd.h for the minix OS. */
#ifndef _UNISTD_H
#define _UNISTD_H

#include <stddef.h>

ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
int close(int fd);
int getpid(void);

#endif
