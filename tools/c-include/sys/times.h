/* sys/times.h for the minix OS — `times()` and the CPU-time struct.
 *
 * `clock_t` is `long`, the type time.h gives it, so `struct tms` keeps its
 * shape wherever `long` does. bash's configure asks for `clock_t` with this
 * header included (BASH_CHECK_TYPE(clock_t, [#include <sys/times.h>], long)),
 * and answers "long" when it is missing — which then collides with the host's
 * own typedef in the build tools. */
#ifndef _SYS_TIMES_H
#define _SYS_TIMES_H

#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

struct tms {
    clock_t tms_utime;  /* user CPU time of the calling process */
    clock_t tms_stime;  /* system CPU time of the calling process */
    clock_t tms_cutime; /* user CPU time of terminated children */
    clock_t tms_cstime; /* system CPU time of terminated children */
};

clock_t times(struct tms *buf);

#ifdef __cplusplus
}
#endif

#endif
