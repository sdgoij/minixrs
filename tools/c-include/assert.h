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
/* libc++ reaches this through <cassert>, so the declaration needs C linkage:
 * without it a C++ caller mangles the name and the link fails. The four
 * arguments match minix-libc's glibc-shaped export. */
#ifdef __cplusplus
extern "C" {
#endif
void __assert_fail(const char *expr, const char *file, unsigned int line,
                   const char *function);
#ifdef __cplusplus
}
#endif
#define assert(expr) \
    ((expr) ? (void)0 : __assert_fail(#expr, __FILE__, __LINE__, __func__))
#endif

#endif
