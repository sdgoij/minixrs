/* fenv.h for the minix OS — the rounding and exception-flag interface.
 *
 * The port fixes the rounding mode at round-to-nearest and has no FP exception
 * trap machinery, so the flag functions report "nothing raised" and the
 * rounding getter returns FE_TONEAREST / the setter accepts and ignores. That is
 * what LLVM libc's floating-point `from_chars` (which libc++'s `charconv.cpp`
 * builds on) needs to compile and link. */
#ifndef _FENV_H
#define _FENV_H

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned long fenv_t;
typedef unsigned short fexcept_t;

#define FE_INVALID 0x01
#define FE_DIVBYZERO 0x04
#define FE_OVERFLOW 0x08
#define FE_UNDERFLOW 0x10
#define FE_INEXACT 0x20
#define FE_ALL_EXCEPT \
    (FE_INVALID | FE_DIVBYZERO | FE_OVERFLOW | FE_UNDERFLOW | FE_INEXACT)

#define FE_TONEAREST 0
#define FE_DOWNWARD 0x400
#define FE_UPWARD 0x800
#define FE_TOWARDZERO 0xC00

#define FE_DFL_ENV ((const fenv_t *)-1)

int feclearexcept(int excepts);
int fetestexcept(int excepts);
int feraiseexcept(int excepts);
int fegetround(void);
int fesetround(int rounding_mode);

#ifdef __cplusplus
}
#endif

#endif
