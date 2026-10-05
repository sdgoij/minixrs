/* endian.h for the minix OS — the byte-order macros only.
 *
 * Mesa's src/util/u_endian.h defines UTIL_ARCH_LITTLE_ENDIAN/BIG_ENDIAN only
 * when <endian.h> supplies a byte-order macro; the port had no such header, so
 * the include was a hard error there. The value comes from clang's predefined
 * __BYTE_ORDER__ rather than a hardcoded architecture, so it follows the target
 * machine. */
#ifndef _ENDIAN_H
#define _ENDIAN_H

#define __LITTLE_ENDIAN 1234
#define __BIG_ENDIAN 4321
#define __PDP_ENDIAN 3412

#if defined(__BYTE_ORDER__) && defined(__ORDER_BIG_ENDIAN__) && \
    (__BYTE_ORDER__ == __ORDER_BIG_ENDIAN__)
#define __BYTE_ORDER __BIG_ENDIAN
#else
#define __BYTE_ORDER __LITTLE_ENDIAN
#endif

#define LITTLE_ENDIAN __LITTLE_ENDIAN
#define BIG_ENDIAN __BIG_ENDIAN
#define PDP_ENDIAN __PDP_ENDIAN
#define BYTE_ORDER __BYTE_ORDER

#endif
