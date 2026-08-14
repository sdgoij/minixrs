/* Minimal sys/socket.h for the minix OS. The socket family is implemented
 * over the net server (minix_std::net); setsockopt is not supported. */
#ifndef _SYS_SOCKET_H
#define _SYS_SOCKET_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned int socklen_t;
typedef unsigned short sa_family_t;

#define AF_UNIX 1
#define AF_INET 2

#define SOCK_STREAM 1
#define SOCK_DGRAM 2
#define SOCK_RAW 3

#define SOL_SOCKET 1
#define SO_PEERCRED 17

struct sockaddr {
    sa_family_t sa_family;
    char sa_data[14];
};

/* Minix sockaddr_in layout (net/gen/socket.h): length byte, family byte,
 * then network-order port/address — matches minix-libc's decoder. */
struct sockaddr_in {
    unsigned char sin_len;
    unsigned char sin_family;
    unsigned short sin_port;
    unsigned int sin_addr;
    char sin_zero[8];
};

struct sockaddr_un {
    sa_family_t sun_family;
    char sun_path[108];
};

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
int getpeername(int fd, struct sockaddr *addr, socklen_t *addrlen);
int getsockname(int fd, struct sockaddr *addr, socklen_t *addrlen);

#ifdef __cplusplus
}
#endif

#endif
