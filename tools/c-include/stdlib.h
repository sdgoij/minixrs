/* Minimal stdlib for the minix OS. */
#ifndef _STDLIB_H
#define _STDLIB_H

#include <stddef.h>

#define MB_CUR_MAX 1

/* Standard exit codes (stdlib.h). */
#define EXIT_SUCCESS 0
#define EXIT_FAILURE 1

#ifdef __cplusplus
extern "C" {
#endif

void *malloc(size_t size);
void free(void *ptr);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void *aligned_alloc(size_t alignment, size_t size);

void exit(int status);
void _Exit(int status);
void abort(void);
int atexit(void (*func)(void));
int abs(int x);
long labs(long x);
long long llabs(long long x);
int atoi(const char *s);
long atol(const char *s);
long long atoll(const char *s);
double atof(const char *s);

long strtol(const char *s, char **endptr, int base);
long long strtoll(const char *s, char **endptr, int base);
unsigned long strtoul(const char *s, char **endptr, int base);
unsigned long long strtoull(const char *s, char **endptr, int base);

float strtof(const char *s, char **endptr);
double strtod(const char *s, char **endptr);
long double strtold(const char *s, char **endptr);

int rand(void);
void srand(unsigned int seed);

/* Multibyte conversions (C locale: one byte == one wide char).
 * Declared here, not wchar.h, matching glibc — libc++'s <cstdlib>
 * imports ::mbtowc and the BSD locale fallbacks call it. */
int mbtowc(wchar_t *pwc, const char *pmb, size_t max);

void qsort(void *base, size_t nmemb, size_t size,
           int (*compar)(const void *, const void *));
void *bsearch(const void *key, const void *base, size_t nmemb, size_t size,
              int (*compar)(const void *, const void *));
char *getenv(const char *name);
int system(const char *command);
char *realpath(const char *path, char *resolved);

/* div/ldiv/lldiv — C standard integer division results. */
typedef struct { int quot, rem; } div_t;
typedef struct { long quot, rem; } ldiv_t;
typedef struct { long long quot, rem; } lldiv_t;
div_t div(int x, int y);
ldiv_t ldiv(long x, long y);
lldiv_t lldiv(long long x, long long y);

#ifdef __cplusplus
}
#endif

#endif
