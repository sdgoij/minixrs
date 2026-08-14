/* Minimal freestanding stddef for the minix OS (x86_64). */
#ifndef _STDDEF_H
#define _STDDEF_H

typedef unsigned long size_t;
typedef long ssize_t;
typedef long ptrdiff_t;
#ifndef __cplusplus
typedef __WCHAR_TYPE__ wchar_t;
#endif

#ifndef NULL
#  ifdef __cplusplus
#    define NULL __null
#  else
#    define NULL ((void *)0)
#  endif
#endif

#define offsetof(type, member) __builtin_offsetof(type, member)

/* C11 max_align_t: the alignment of the largest fundamental type
 * (long double on x86-64 — 16 bytes). libc++ uses alignof(max_align_t)
 * for its pool allocators. */
typedef struct {
    long long __max_align_ll;
    long double __max_align_ld;
} max_align_t;

#endif
