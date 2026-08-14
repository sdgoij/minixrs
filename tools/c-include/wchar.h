/* Minimal wchar.h for the minix OS — mbstate_t plus the wchar functions
 * libc++'s char_traits<wchar_t> needs. Includes stdio.h for FILE (the
 * wide I/O functions take FILE*, and libc++'s <cwchar> imports ::FILE)
 * and time.h for struct tm (wcsftime's argument; glibc's wchar.h does
 * the same so libc++'s <cwchar> `using ::tm` import always resolves). */
#ifndef _WCHAR_H
#define _WCHAR_H

#include <stddef.h>
#include <stdio.h>
#include <time.h>

typedef struct { unsigned long __state; } mbstate_t;
typedef unsigned int wint_t;
#define WEOF ((wint_t)-1)

#ifdef __cplusplus
extern "C" {
#endif

wchar_t *wcscpy(wchar_t *dst, const wchar_t *src);
wchar_t *wcsncpy(wchar_t *dst, const wchar_t *src, size_t n);
wchar_t *wcscat(wchar_t *dst, const wchar_t *src);
wchar_t *wcsncat(wchar_t *dst, const wchar_t *src, size_t n);
int wcscmp(const wchar_t *a, const wchar_t *b);
int wcsncmp(const wchar_t *a, const wchar_t *b, size_t n);
size_t wcslen(const wchar_t *s);
wchar_t *wcschr(const wchar_t *s, wchar_t c);
wchar_t *wcsrchr(const wchar_t *s, wchar_t c);
wchar_t *wcsstr(const wchar_t *haystack, const wchar_t *needle);
wchar_t *wcspbrk(const wchar_t *s, const wchar_t *accept);
size_t wcsspn(const wchar_t *s, const wchar_t *accept);
size_t wcscspn(const wchar_t *s, const wchar_t *reject);
void *wmemcpy(void *dst, const void *src, size_t n);
void *wmemmove(void *dst, const void *src, size_t n);
void *wmemset(void *s, wchar_t c, size_t n);
int wmemcmp(const void *a, const void *b, size_t n);
wchar_t *wmemchr(const wchar_t *s, wchar_t c, size_t n);
long wcstol(const wchar_t *s, wchar_t **endptr, int base);
unsigned long wcstoul(const wchar_t *s, wchar_t **endptr, int base);
long long wcstoll(const wchar_t *s, wchar_t **endptr, int base);
unsigned long long wcstoull(const wchar_t *s, wchar_t **endptr, int base);
float wcstof(const wchar_t *s, wchar_t **endptr);
double wcstod(const wchar_t *s, wchar_t **endptr);
long double wcstold(const wchar_t *s, wchar_t **endptr);
int swprintf(wchar_t *ws, size_t n, const wchar_t *fmt, ...);
int vswprintf(wchar_t *ws, size_t n, const wchar_t *fmt, va_list ap);

/* Wide FILE I/O (C locale: one byte == one wide char). libc++'s
 * std_stream.h calls getwc/ungetwc/fputwc; getwc needs FILE and WEOF. */
wint_t fgetwc(FILE *stream);
wint_t getwc(FILE *stream);
wint_t getwchar(void);
wint_t fputwc(wchar_t wc, FILE *stream);
wint_t putwc(wchar_t wc, FILE *stream);
wint_t putwchar(wchar_t wc);
wint_t ungetwc(wint_t wc, FILE *stream);

/* Multibyte conversions (C locale: one byte == one wide char). */
wint_t btowc(int c);
int wctob(wint_t c);
size_t wcrtomb(char *s, wchar_t wc, mbstate_t *ps);
size_t mbrtowc(wchar_t *pwc, const char *s, size_t n, mbstate_t *ps);
int mbtowc(wchar_t *pwc, const char *pmb, size_t max);
size_t mbrlen(const char *s, size_t n, mbstate_t *ps);
size_t mbsrtowcs(wchar_t *dest, const char **src, size_t len, mbstate_t *ps);
size_t mbsnrtowcs(wchar_t *dest, const char **src, size_t nms, size_t len, mbstate_t *ps);
size_t wcsnrtombs(char *dest, const wchar_t **src, size_t nwc, size_t len, mbstate_t *ps);
size_t wcsrtombs(char *dest, const wchar_t **src, size_t len, mbstate_t *ps);
int wcscoll(const wchar_t *s1, const wchar_t *s2);
size_t wcsxfrm(wchar_t *dest, const wchar_t *src, size_t n);

#ifdef __cplusplus
}
#endif

#endif
