/* Minimal stdio for the minix OS — unbuffered stdout on fd 1. */
#ifndef _STDIO_H
#define _STDIO_H

#include <stddef.h>

typedef __builtin_va_list va_list;
#define va_start(ap, last) __builtin_va_start(ap, last)
#define va_end(ap) __builtin_va_end(ap)
#define va_arg(ap, type) __builtin_va_arg(ap, type)

#ifdef __cplusplus
extern "C" {
#endif

int putchar(int c);
int puts(const char *s);
int printf(const char *fmt, ...);
int vprintf(const char *fmt, va_list ap);

#ifdef __cplusplus
}
#endif

/* FILE-based stdio. All streams are unbuffered and currently route to the
 * serial console (fopen-family I/O lands on real fds once implemented). */
typedef struct __FILE FILE;
typedef long fpos_t;
#define EOF (-1)
#define BUFSIZ 8192

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

#ifdef __cplusplus
extern "C" {
#endif

extern FILE *stdin;
extern FILE *stdout;
extern FILE *stderr;

int fprintf(FILE *stream, const char *fmt, ...);
int vfprintf(FILE *stream, const char *fmt, va_list ap);
int fputs(const char *s, FILE *stream);
char *fgets(char *s, int size, FILE *stream);
int fputc(int c, FILE *stream);
int putc(int c, FILE *stream);
int fgetc(FILE *stream);
int getc(FILE *stream);
int getchar(void);
int fflush(FILE *stream);
int fclose(FILE *stream);
int fileno(FILE *stream);

FILE *fopen(const char *path, const char *mode);
FILE *freopen(const char *path, const char *mode, FILE *stream);
size_t fread(void *ptr, size_t size, size_t nmemb, FILE *stream);
size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *stream);
int fseek(FILE *stream, long offset, int whence);
long ftell(FILE *stream);
void rewind(FILE *stream);
int feof(FILE *stream);
int ferror(FILE *stream);
void clearerr(FILE *stream);
int ungetc(int c, FILE *stream);
int remove(const char *path);
int rename(const char *oldpath, const char *newpath);
int perror(const char *s);
int sprintf(char *str, const char *fmt, ...);
int snprintf(char *str, size_t size, const char *fmt, ...);
int vsprintf(char *str, const char *fmt, va_list ap);
int vsnprintf(char *str, size_t size, const char *fmt, va_list ap);
int vasprintf(char **strp, const char *fmt, va_list ap);
int scanf(const char *fmt, ...);
int sscanf(const char *str, const char *fmt, ...);
int fscanf(FILE *stream, const char *fmt, ...);

#ifdef __cplusplus
}
#endif

#endif
