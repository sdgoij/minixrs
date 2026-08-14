/* Minimal math.h for the minix OS — classification constants only.
 * The libc++ <math.h> wrapper provides the math functions themselves
 * via compiler builtins when the C library lacks them. */
#ifndef _MATH_H
#define _MATH_H

#define INFINITY (__builtin_inff())
#define NAN (__builtin_nanf(""))
#define HUGE_VAL (__builtin_huge_val())
#define HUGE_VALF (__builtin_huge_valf())
#define HUGE_VALL (__builtin_huge_vall())

/* Distinct values for __builtin_fpclassify; the numeric values are
 * implementation-defined. */
#define FP_NAN 0
#define FP_INFINITE 1
#define FP_ZERO 2
#define FP_SUBNORMAL 3
#define FP_NORMAL 4

#endif
