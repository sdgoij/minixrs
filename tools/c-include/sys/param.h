/* sys/param.h for the minix OS — the constants C code reaches for out of habit.
 *
 * It includes unistd.h because that is where this port keeps PATH_MAX: glibc's
 * sys/param.h takes MAXPATHLEN from <limits.h>, and the port has no limits.h,
 * so the two would drift if this header defined its own copy. */
#ifndef _SYS_PARAM_H
#define _SYS_PARAM_H

#include <unistd.h>

#define MAXPATHLEN PATH_MAX
#define MAXNAMLEN 255

#ifndef NBBY
#define NBBY 8 /* bits per byte */
#endif

#define howmany(x, y) (((x) + ((y) - 1)) / (y))
#define roundup(x, y) (((x) + ((y) - 1)) & ~((y) - 1))

#endif
