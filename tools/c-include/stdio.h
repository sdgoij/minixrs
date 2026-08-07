/* Minimal stdio for the minix OS — unbuffered stdout on fd 1. */
#ifndef _STDIO_H
#define _STDIO_H

#include <stddef.h>

typedef __builtin_va_list va_list;
#define va_start(ap, last) __builtin_va_start(ap, last)
#define va_end(ap) __builtin_va_end(ap)
#define va_arg(ap, type) __builtin_va_arg(ap, type)

int putchar(int c);
int puts(const char *s);
int printf(const char *fmt, ...);
int vprintf(const char *fmt, va_list ap);

#endif
