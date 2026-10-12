/* Minimal sys/socket.h for the minix OS. The socket family is implemented
 * over the net server (minix_std::net); setsockopt is not supported. */
#ifndef _SYS_SOCKET_H
#define _SYS_SOCKET_H

#include <stddef.h>
#include <sys/types.h>
#include <sys/uio.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned int socklen_t;
typedef unsigned short sa_family_t;

#define AF_UNIX 1
#define AF_INET 2
#define AF_LOCAL AF_UNIX
#define PF_UNIX AF_UNIX
#define PF_LOCAL AF_UNIX

#define SOCK_STREAM 1
#define SOCK_DGRAM 2
#define SOCK_RAW 3

/* MINIX's `SOL_SOCKET` is 0xffff (`sys/sys/socket.h`), not Linux's 1. */
#define SOL_SOCKET 0xffff
#define SO_PEERCRED 17

struct sockaddr {
    sa_family_t sa_family;
    char sa_data[14];
};

/* The 32-bit IPv4 address, holding the four octets in network order. Spelled as
 * a struct, as POSIX does, because C code assigns through `.s_addr`. */
struct in_addr {
    unsigned int s_addr;
};

/* Minix sockaddr_in layout (net/gen/socket.h): length byte, family byte,
 * then network-order port/address — matches minix-libc's decoder.
 *
 * sin_family is one byte and sin_len leads, which MINIX keeps from its BSD
 * ancestry; code that assigns AF_INET still compiles (it truncates). */
struct sockaddr_in {
    unsigned char sin_len;
    unsigned char sin_family;
    unsigned short sin_port;
    struct in_addr sin_addr;
    char sin_zero[8];
};

struct sockaddr_un {
    sa_family_t sun_family;
    char sun_path[108];
};

/* Ancillary data (`sendmsg`/`recvmsg`). The layouts are MINIX's
 * (`sys/sys/socket.h`): a 32-bit `socklen_t` and an alignment of 4, so
 * `struct cmsghdr` is 12 bytes and `CMSG_DATA` starts at 12 — the numbers the
 * uds server and `net::uds` use. */
#define SCM_RIGHTS 0x01
#define SCM_CREDS 0x04

#define MSG_OOB 0x0001
#define MSG_PEEK 0x0002
#define MSG_DONTROUTE 0x0004
#define MSG_EOR 0x0008
#define MSG_TRUNC 0x0010
#define MSG_CTRUNC 0x0020
#define MSG_WAITALL 0x0040
#define MSG_DONTWAIT 0x0080

struct cmsghdr {
    socklen_t cmsg_len;
    int cmsg_level;
    int cmsg_type;
};

struct msghdr {
    void *msg_name;
    socklen_t msg_namelen;
    struct iovec *msg_iov;
    int msg_iovlen;
    void *msg_control;
    socklen_t msg_controllen;
    int msg_flags;
};

/* `__CMSG_ALIGN` is MINIX's `(n + __ALIGNBYTES) & ~__ALIGNBYTES`, with
 * `__ALIGNBYTES == sizeof(int) - 1`. */
#define __CMSG_ALIGN(n) (((n) + 3u) & ~3u)
#define CMSG_ALIGN(n) __CMSG_ALIGN(n)
#define CMSG_DATA(cmsg) \
    ((unsigned char *)(cmsg) + __CMSG_ALIGN(sizeof(struct cmsghdr)))
#define CMSG_SPACE(l) (__CMSG_ALIGN(sizeof(struct cmsghdr)) + __CMSG_ALIGN(l))
#define CMSG_LEN(l) (__CMSG_ALIGN(sizeof(struct cmsghdr)) + (l))
#define CMSG_FIRSTHDR(mhdr) \
    ((mhdr)->msg_controllen >= sizeof(struct cmsghdr) \
         ? (struct cmsghdr *)(mhdr)->msg_control \
         : (struct cmsghdr *)0)
#define CMSG_NXTHDR(mhdr, cmsg) \
    (((char *)(cmsg) + __CMSG_ALIGN((cmsg)->cmsg_len) + \
          __CMSG_ALIGN(sizeof(struct cmsghdr)) > \
      ((char *)(mhdr)->msg_control) + (mhdr)->msg_controllen) \
         ? (struct cmsghdr *)0 \
         : (struct cmsghdr *)(void *)((char *)(cmsg) + \
                                      __CMSG_ALIGN((cmsg)->cmsg_len)))

int socket(int domain, int type, int protocol);
int bind(int fd, const struct sockaddr *addr, socklen_t addrlen);
int connect(int fd, const struct sockaddr *addr, socklen_t addrlen);
int listen(int fd, int backlog);
int accept(int fd, struct sockaddr *addr, socklen_t *addrlen);
int shutdown(int fd, int how);
int setsockopt(int fd, int level, int optname, const void *optval, socklen_t optlen);
ssize_t send(int fd, const void *buf, size_t len, int flags);
ssize_t sendto(int fd, const void *buf, size_t len, int flags,
               const struct sockaddr *dest_addr, socklen_t dest_len);
ssize_t recv(int fd, void *buf, size_t len, int flags);
ssize_t recvfrom(int fd, void *buf, size_t len, int flags,
                 struct sockaddr *src_addr, socklen_t *src_len);
ssize_t sendmsg(int fd, const struct msghdr *msg, int flags);
ssize_t recvmsg(int fd, struct msghdr *msg, int flags);
int getpeername(int fd, struct sockaddr *addr, socklen_t *addrlen);
int getsockname(int fd, struct sockaddr *addr, socklen_t *addrlen);

#ifdef __cplusplus
}
#endif

#endif
