/* Minimal poll.h for the minix OS — poll() is a stub (no readiness
 * notification in the net server yet); fds 0-2 (serial) are always
 * ready. */
#ifndef _POLL_H
#define _POLL_H

#ifdef __cplusplus
extern "C" {
#endif

struct pollfd {
    int fd;
    short events;
    short revents;
};

#define POLLIN 0x001
#define POLLPRI 0x002
#define POLLOUT 0x004
#define POLLERR 0x008
#define POLLHUP 0x010
#define POLLNVAL 0x020

int poll(struct pollfd *fds, unsigned long nfds, int timeout);

#ifdef __cplusplus
}
#endif

#endif
