/* Minimal assert.h for the minix OS. */
#ifndef _ASSERT_H
#define _ASSERT_H

/* C11 `static_assert`, over the `_Static_assert` keyword. C++11 has it as a
 * keyword, so this must not shadow that. */
#ifndef __cplusplus
#ifndef static_assert
#define static_assert _Static_assert
#endif
#endif

#ifdef NDEBUG
#define assert(expr) ((void)0)
#else
void __assert_fail(const char *expr, const char *file, int line);
#define assert(expr) \
    ((expr) ? (void)0 : __assert_fail(#expr, __FILE__, __LINE__))
#endif

#endif
