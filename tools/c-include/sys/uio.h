/* Scatter/gather I/O (minix): `struct iovec` and the vector read/write calls.
 * The layouts match MINIX's `sys/sys/uio.h` so a C caller's structures are the
 * ones the C library and the drivers agree on. */
#ifndef _SYS_UIO_H
#define _SYS_UIO_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

struct iovec {
    void *iov_base;
    size_t iov_len;
};

ssize_t readv(int fd, const struct iovec *iov, int iovcnt);
ssize_t writev(int fd, const struct iovec *iov, int iovcnt);

#ifdef __cplusplus
}
#endif

#endif
