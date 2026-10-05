/* select(2) for the minix OS. An `fd_set` in this port is a single 64-bit
 * bitmask representing fds 0..63, which is what the VFS readiness engine
 * (crates/servers/src/vfs/select.rs) reads and writes. `select` supports a real
 * timeout; see also poll.h. */
#ifndef _SYS_SELECT_H
#define _SYS_SELECT_H

#include <sys/time.h>

/* glibc's <sys/select.h> pulls in `sigset_t` for its `pselect` declaration;
 * readline's posixselect.h includes this header and then rlprivate.h declares
 * `_rl_timeout_select(..., const sigset_t *)`, so the type has to be visible
 * here too. This port has no `pselect`, but the type still comes from the
 * header that owns it. */
#include <signal.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned long fd_set;

#define FD_SETSIZE 64

#define FD_ZERO(p) (*(p) = 0)
#define FD_SET(n, p) (*(p) |= (1UL << (n)))
#define FD_CLR(n, p) (*(p) &= ~(1UL << (n)))
#define FD_ISSET(n, p) ((*(p) & (1UL << (n))) != 0)

/* `timeout` is NULL to block forever, a zero `struct timeval` to poll, or a
 * deadline. Returns the number of ready fds, 0 on timeout, or -1 with errno. */
int select(int nfds, fd_set *readfds, fd_set *writefds, fd_set *exceptfds,
           struct timeval *timeout);

#ifdef __cplusplus
}
#endif

#endif
