/* math.h for the minix OS.
 *
 * The classification macros are clang builtins, so they need no libm entry
 * point. The functions themselves are the C ABI wrappers in minix-libc's
 * `c_math.rs`, over the pure-Rust `libm`. */
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

/* The X/Open math constants. glibc exposes them under _DEFAULT_SOURCE; Mesa's
 * util and the GLSL compiler use M_PI and M_LOG2E unconditionally. */
#define M_E 2.7182818284590452354
#define M_LOG2E 1.4426950408889634074
#define M_LOG10E 0.43429448190325182765
#define M_LN2 0.69314718055994530942
#define M_LN10 2.30258509299404568402
#define M_PI 3.14159265358979323846
#define M_PI_2 1.57079632679489661923
#define M_PI_4 0.78539816339744830962
#define M_1_PI 0.31830988618379067154
#define M_2_PI 0.63661977236758134308
#define M_2_SQRTPI 1.12837916709551257390
#define M_SQRT2 1.41421356237309504880
#define M_SQRT1_2 0.70710678118654752440

/* Radix-independent exponent, by IEEE bit extraction rather than `log()` —
 * `no_std` has no logarithm. */
double logb(double x);

/* The C99 classification macros, over clang's builtins so no libm entry point is
 * needed. `fpclassify` reuses the `FP_*` values above. */
#define fpclassify(x) __builtin_fpclassify(FP_NAN, FP_INFINITE, FP_NORMAL, FP_SUBNORMAL, FP_ZERO, (x))
#define isnan(x) __builtin_isnan(x)
#define isinf(x) __builtin_isinf(x)
#define isfinite(x) __builtin_isfinite(x)
#define isnormal(x) __builtin_isnormal(x)
#define signbit(x) __builtin_signbit(x)

#ifdef __cplusplus
extern "C" {
#endif

double fabs(double x);
float fabsf(float x);
double fmin(double x, double y);
float fminf(float x, float y);
double fmax(double x, double y);
float fmaxf(float x, float y);

double floor(double x);
float floorf(float x);
double ceil(double x);
float ceilf(float x);
double trunc(double x);
float truncf(float x);
double round(double x);
float roundf(float x);
double rint(double x);
float rintf(float x);
double nearbyint(double x);
float nearbyintf(float x);
long int lrint(double x);
long int lrintf(float x);
long long int llrint(double x);
long long int llrintf(float x);
long int lround(double x);
long int lroundf(float x);
long long int llround(double x);
long long int llroundf(float x);

double sqrt(double x);
float sqrtf(float x);
double cbrt(double x);
float cbrtf(float x);
double hypot(double x, double y);
float hypotf(float x, float y);

double exp(double x);
float expf(float x);
double exp2(double x);
float exp2f(float x);
double expm1(double x);
float expm1f(float x);
double log(double x);
float logf(float x);
double log2(double x);
float log2f(float x);
double log10(double x);
float log10f(float x);
double log1p(double x);
float log1pf(float x);

double sin(double x);
float sinf(float x);
double cos(double x);
float cosf(float x);
double tan(double x);
float tanf(float x);
double asin(double x);
float asinf(float x);
double acos(double x);
float acosf(float x);
double atan(double x);
float atanf(float x);
double atan2(double y, double x);
float atan2f(float y, float x);
double sinh(double x);
float sinhf(float x);
double cosh(double x);
float coshf(float x);
double tanh(double x);
float tanhf(float x);

double pow(double x, double y);
float powf(float x, float y);
double fmod(double x, double y);
float fmodf(float x, float y);
double fma(double x, double y, double z);
float fmaf(float x, float y, float z);
double fdim(double x, double y);
float fdimf(float x, float y);
double copysign(double x, double y);
float copysignf(float x, float y);
double nextafter(double x, double y);
float nextafterf(float x, float y);

double ldexp(double x, int exp);
float ldexpf(float x, int exp);
double scalbn(double x, int exp);
float scalbnf(float x, int exp);
double frexp(double x, int *exp);
float frexpf(float x, int *exp);
double modf(double x, double *iptr);
float modff(float x, float *iptr);

double nan(const char *tagp);
float nanf(const char *tagp);

#ifdef __cplusplus
}
#endif

#endif
