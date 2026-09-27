/* timerfd(3) for the minix OS. The object is VFS-internal (no filesystem, no
 * path): a descriptor that becomes readable when a timer expires, so it can sit
 * in the same poll/select as sockets. crates/servers/src/vfs/timerfd.rs. */
#ifndef _SYS_TIMERFD_H
#define _SYS_TIMERFD_H

#include <time.h>
#include <fcntl.h>

#ifdef __cplusplus
extern "C" {
#endif

struct itimerspec {
    struct timespec it_interval; /* period; zero for a one-shot timer */
    struct timespec it_value;    /* first expiry; all-zero to disarm */
};

#define TFD_CLOEXEC O_CLOEXEC
#define TFD_NONBLOCK O_NONBLOCK
/* it_value is an absolute time on the timer's clock. */
#define TFD_TIMER_ABSTIME 1

int timerfd_create(int clockid, int flags);
int timerfd_settime(int fd, int flags, const struct itimerspec *new_value,
                    struct itimerspec *old_value);
int timerfd_gettime(int fd, struct itimerspec *curr_value);

#ifdef __cplusplus
}
#endif

#endif
