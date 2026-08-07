/* Minimal freestanding stddef for the minix OS (x86_64). */
#ifndef _STDDEF_H
#define _STDDEF_H

typedef unsigned long size_t;
typedef long ssize_t;
typedef long ptrdiff_t;

#define NULL ((void *)0)

#endif
