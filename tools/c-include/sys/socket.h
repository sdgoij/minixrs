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
#define AF_INET6 10
#define AF_LOCAL AF_UNIX
#define PF_UNIX AF_UNIX
#define PF_LOCAL AF_UNIX

#define SOCK_STREAM 1
#define SOCK_DGRAM 2
#define SOCK_RAW 3
#define SOCK_RDM 4
#define SOCK_SEQPACKET 5
/* ORed into the type, as on Linux. */
#define SOCK_CLOEXEC 0x80000
#define SOCK_NONBLOCK 0x800

/* MINIX's `SOL_SOCKET` is 0xffff (`sys/sys/socket.h`), not Linux's 1. */
#define SOL_SOCKET 0xffff
#define SO_REUSEADDR 2
#define SO_TYPE 3
#define SO_ERROR 4
#define SO_BROADCAST 6
#define SO_SNDBUF 7
#define SO_RCVBUF 8
#define SO_KEEPALIVE 9
#define SO_OOBINLINE 10
#define SO_LINGER 13
#define SO_REUSEPORT 15
#define SO_PEERCRED 17
#define SO_RCVTIMEO 20
#define SO_SNDTIMEO 21
#define SO_ACCEPTCONN 30
#define SO_DOMAIN 39

struct sockaddr {
    sa_family_t sa_family;
    char sa_data[14];
};

/* The 32-bit IPv4 address, holding the four octets in network order. Spelled as
 * a struct, as POSIX does, because C code assigns through `.s_addr`. */
struct in_addr {
    unsigned int s_addr;
};

/* POSIX layout: the family comes first. MINIX's own header keeps a BSD
 * `sin_len` byte ahead of it; the port's decoder (`minix-libc`) and these
 * declarations share one layout, and nothing outside them builds these bytes.
 * sin_port/sin_addr are in network order. */
struct sockaddr_in {
    sa_family_t sin_family;
    unsigned short sin_port;
    struct in_addr sin_addr;
    char sin_zero[8];
};

struct in6_addr {
    unsigned char s6_addr[16];
};

struct sockaddr_in6 {
    sa_family_t sin6_family;
    unsigned short sin6_port;
    unsigned int sin6_flowinfo;
    struct in6_addr sin6_addr;
    unsigned int sin6_scope_id;
};

struct ip_mreq {
    struct in_addr imr_multiaddr;
    struct in_addr imr_interface;
};

struct ipv6_mreq {
    struct in6_addr ipv6mr_multiaddr;
    unsigned int ipv6mr_interface;
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
#define MSG_CONFIRM 0x0800
#define MSG_ERRQUEUE 0x2000
#define MSG_NOSIGNAL 0x4000
#define MSG_MORE 0x8000
#define MSG_CMSG_CLOEXEC 0x40000000

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
int accept4(int fd, struct sockaddr *addr, socklen_t *addrlen, int flags);
int shutdown(int fd, int how);
int setsockopt(int fd, int level, int optname, const void *optval, socklen_t optlen);
int getsockopt(int fd, int level, int optname, void *optval, socklen_t *optlen);
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
