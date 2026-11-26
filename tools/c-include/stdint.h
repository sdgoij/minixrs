/* Freestanding stdint for the minix OS.
 *
 * The 64-bit types and the pointer-sized ones come from the compiler's own width
 * macros rather than a written-down `long`. This header is compiled for both the
 * LP64 arches and wasm32 (ILP32), and `long` is 64 bits on one and 32 on the
 * other: `typedef unsigned long uint64_t` gave wasm32 a 32-bit `uint64_t`, so a
 * struct a C program passed across an ioctl was half the size the Rust side read
 * and the argument decoded as garbage. The macros are the same types on LP64
 * (`long`/`unsigned long`), so nothing changes for the archs that were right.
 */
#ifndef _STDINT_H
#define _STDINT_H

typedef signed char int8_t;
typedef unsigned char uint8_t;
typedef short int16_t;
typedef unsigned short uint16_t;
typedef int int32_t;
typedef unsigned int uint32_t;
typedef __INT64_TYPE__ int64_t;
typedef __UINT64_TYPE__ uint64_t;

typedef __INTPTR_TYPE__ intptr_t;
typedef __UINTPTR_TYPE__ uintptr_t;

typedef int8_t int_least8_t;
typedef uint8_t uint_least8_t;
typedef int16_t int_least16_t;
typedef uint16_t uint_least16_t;
typedef int32_t int_least32_t;
typedef uint32_t uint_least32_t;
typedef int64_t int_least64_t;
typedef uint64_t uint_least64_t;

typedef int8_t int_fast8_t;
typedef uint8_t uint_fast8_t;
typedef int64_t int_fast16_t;
typedef uint64_t uint_fast16_t;
typedef int64_t int_fast32_t;
typedef uint64_t uint_fast32_t;
typedef int64_t int_fast64_t;
typedef uint64_t uint_fast64_t;

typedef int64_t intmax_t;
typedef uint64_t uintmax_t;

#define INT8_MIN (-128)
#define INT8_MAX 127
#define UINT8_MAX 255U
#define INT16_MIN (-32767 - 1)
#define INT16_MAX 32767
#define UINT16_MAX 65535U
#define INT32_MIN (-2147483647 - 1)
#define INT32_MAX 2147483647
#define UINT32_MAX 4294967295U
#define INT64_MIN (-__INT64_MAX__ - 1)
#define INT64_MAX __INT64_MAX__
#define UINT64_MAX __UINT64_MAX__

#define _STDINT_CAT_(a, b) a##b
#define _STDINT_CAT(a, b) _STDINT_CAT_(a, b)

#define INT8_C(x) x
#define INT16_C(x) x
#define INT32_C(x) x
#define INT64_C(x) _STDINT_CAT(x, __INT64_C_SUFFIX__)
#define UINT8_C(x) x
#define UINT16_C(x) x
#define UINT32_C(x) x##U
#define UINT64_C(x) _STDINT_CAT(x, __UINT64_C_SUFFIX__)
#define INTMAX_C(x) INT64_C(x)
#define UINTMAX_C(x) UINT64_C(x)

#define SIZE_MAX __SIZE_MAX__

#define INTPTR_MIN (-__INTPTR_MAX__ - 1)
#define INTMAX_MIN INT64_MIN
#define INTMAX_MAX INT64_MAX
#define UINTMAX_MAX UINT64_MAX
#define INTPTR_MAX __INTPTR_MAX__
#define UINTPTR_MAX __UINTPTR_MAX__
#define PTRDIFF_MIN (-__PTRDIFF_MAX__ - 1)
#define PTRDIFF_MAX __PTRDIFF_MAX__

#endif
