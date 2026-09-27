/* epoll(3) for the minix OS. An epoll instance is VFS-internal (no filesystem,
 * no path): a descriptor holding a persistent interest set, so an event loop can
 * wait on many sources at once alongside poll/select.
 * crates/servers/src/vfs/epoll.rs. */
#ifndef _SYS_EPOLL_H
#define _SYS_EPOLL_H

#include <stdint.h>
#include <fcntl.h>

#ifdef __cplusplus
extern "C" {
#endif

/* The kernel ABI, as on Linux: packed, so this is 12 bytes, not 16. */
struct epoll_event {
    uint32_t events;
    uint64_t data;
} __attribute__((__packed__));

#define EPOLL_CLOEXEC O_CLOEXEC

#define EPOLL_CTL_ADD 1
#define EPOLL_CTL_DEL 2
#define EPOLL_CTL_MOD 3

#define EPOLLIN 0x001
#define EPOLLPRI 0x002
#define EPOLLOUT 0x004
#define EPOLLERR 0x008
#define EPOLLHUP 0x010
#define EPOLLNVAL 0x020
#define EPOLLRDHUP 0x2000
/* Accepted, but served level-triggered (see minixrs WAYLAND.md). */
#define EPOLLET (1u << 31)
#define EPOLLONESHOT (1u << 30)

int epoll_create(int size);
int epoll_create1(int flags);
int epoll_ctl(int epfd, int op, int fd, struct epoll_event *event);
int epoll_wait(int epfd, struct epoll_event *events, int maxevents, int timeout);

#ifdef __cplusplus
}
#endif

#endif
