/* Minimal assert.h for the minix OS. */
#ifndef _ASSERT_H
#define _ASSERT_H

#ifdef NDEBUG
#define assert(expr) ((void)0)
#else
void __assert_fail(const char *expr, const char *file, int line);
#define assert(expr) \
    ((expr) ? (void)0 : __assert_fail(#expr, __FILE__, __LINE__))
#endif

#endif
