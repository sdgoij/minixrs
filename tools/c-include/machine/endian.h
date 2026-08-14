/* Minimal machine/endian.h for the minix OS — x86_64 is little-endian. */
#ifndef _MACHINE_ENDIAN_H
#define _MACHINE_ENDIAN_H

#define BIG_ENDIAN 4321
#define LITTLE_ENDIAN 1234
#define BYTE_ORDER LITTLE_ENDIAN

static inline unsigned short __bswap16(unsigned short x) {
    return __builtin_bswap16(x);
}

static inline unsigned int __bswap32(unsigned int x) {
    return __builtin_bswap32(x);
}

static inline unsigned long long __bswap64(unsigned long long x) {
    return __builtin_bswap64(x);
}

#endif
