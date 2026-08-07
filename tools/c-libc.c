/* C half of the minix libc, compiled freestanding by build-c-hello.py.
 *
 * Rust's minix-libc (crates/minix-libc) provides the syscall surface
 * (open/read/write/.../malloc/errno) as C ABI; this file adds the pieces
 * that need C varargs (stdio) plus common string/stdlib helpers, all
 * writing through minix-libc's `write` on fd 1 (unbuffered stdout).
 */

typedef unsigned long size_t;
typedef long ssize_t;

#define va_list __builtin_va_list
#define va_start(ap, last) __builtin_va_start(ap, last)
#define va_end(ap) __builtin_va_end(ap)
#define va_arg(ap, type) __builtin_va_arg(ap, type)

extern ssize_t write(int fd, const void *buf, size_t count);
extern size_t strlen(const char *s);

/* ---- stdio (unbuffered, fd 1) ---- */

int putchar(int c) {
    char b = (char)c;
    if (write(1, &b, 1) == 1) return (unsigned char)c;
    return -1;
}

int puts(const char *s) {
    size_t n = strlen(s);
    if (write(1, s, n) != (ssize_t)n) return -1;
    return putchar('\n');
}

static void pad(int ch, int n) {
    while (n-- > 0) putchar(ch);
}

static int emit_num(unsigned long v, int base, const char *digits,
                    int neg, int left, int zero, int width) {
    char buf[40];
    int n = 0;
    do {
        buf[n++] = digits[v % base];
        v /= base;
    } while (v);
    int padn = width - n - neg;
    if (!left && zero) {
        if (neg) putchar('-');
        pad('0', padn);
    } else {
        if (!left) pad(' ', padn);
        if (neg) putchar('-');
    }
    for (int i = n - 1; i >= 0; i--) putchar(buf[i]);
    if (left) pad(' ', padn);
    return n + neg;
}

int vprintf(const char *fmt, va_list ap) {
    int count = 0;
    for (const char *p = fmt; *p; p++) {
        if (*p != '%') {
            putchar((unsigned char)*p);
            count++;
            continue;
        }
        p++;
        int left = 0, zero = 0, width = 0;
        while (*p == '-' || *p == '0') {
            if (*p == '-') left = 1; else zero = 1;
            p++;
        }
        while (*p >= '0' && *p <= '9') {
            width = width * 10 + (*p - '0');
            p++;
        }
        unsigned long v;
        switch (*p) {
        case '%': putchar('%'); count++; break;
        case 'c': putchar(va_arg(ap, int)); count++; break;
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s) s = "(null)";
            size_t len = strlen(s);
            int padn = width > (int)len ? width - (int)len : 0;
            if (!left) pad(' ', padn);
            for (size_t i = 0; i < len; i++) putchar((unsigned char)s[i]);
            if (left) pad(' ', padn);
            count += (int)len;
            break;
        }
        case 'p': v = (unsigned long)va_arg(ap, void *); count += emit_num(v, 16, "0123456789abcdef", 0, left, zero, width); break;
        case 'x': v = va_arg(ap, unsigned int); count += emit_num(v, 16, "0123456789abcdef", 0, left, zero, width); break;
        case 'X': v = va_arg(ap, unsigned int); count += emit_num(v, 16, "0123456789ABCDEF", 0, left, zero, width); break;
        case 'u': v = va_arg(ap, unsigned int); count += emit_num(v, 10, "0123456789", 0, left, zero, width); break;
        case 'd': case 'i': {
            long l = va_arg(ap, int);
            if (l < 0) { v = (unsigned long)(-(l + 1)) + 1; count += emit_num(v, 10, "0123456789", 1, left, zero, width); }
            else { v = (unsigned long)l; count += emit_num(v, 10, "0123456789", 0, left, zero, width); }
            break;
        }
        case 'l': {
            p++;
            switch (*p) {
            case 'd': case 'i': {
                long l = va_arg(ap, long);
                if (l < 0) { v = (unsigned long)(-(l + 1)) + 1; count += emit_num(v, 10, "0123456789", 1, left, zero, width); }
                else { v = (unsigned long)l; count += emit_num(v, 10, "0123456789", 0, left, zero, width); }
                break;
            }
            case 'u': v = va_arg(ap, unsigned long); count += emit_num(v, 10, "0123456789", 0, left, zero, width); break;
            case 'x': v = va_arg(ap, unsigned long); count += emit_num(v, 16, "0123456789abcdef", 0, left, zero, width); break;
            case 'X': v = va_arg(ap, unsigned long); count += emit_num(v, 16, "0123456789ABCDEF", 0, left, zero, width); break;
            default: putchar('%'); putchar('l'); count += 2; break;
            }
            break;
        }
        case 'z': {
            p++;
            switch (*p) {
            case 'u': v = (unsigned long)va_arg(ap, size_t); count += emit_num(v, 10, "0123456789", 0, left, zero, width); break;
            case 'd': case 'i': {
                long l = (long)va_arg(ap, ssize_t);
                if (l < 0) { v = (unsigned long)(-(l + 1)) + 1; count += emit_num(v, 10, "0123456789", 1, left, zero, width); }
                else { v = (unsigned long)l; count += emit_num(v, 10, "0123456789", 0, left, zero, width); }
                break;
            }
            default: putchar('%'); putchar('z'); count += 2; break;
            }
            break;
        }
        default: putchar('%'); putchar(*p); count += 2; break;
        }
    }
    return count;
}

int printf(const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int n = vprintf(fmt, ap);
    va_end(ap);
    return n;
}

/* ---- string.h extras ---- */

int memcmp(const void *a, const void *b, size_t n) {
    const unsigned char *x = a, *y = b;
    for (size_t i = 0; i < n; i++) {
        if (x[i] != y[i]) return x[i] - y[i];
    }
    return 0;
}

int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n) {
    while (n-- && *a && *a == *b) { a++; b++; }
    if (n == (size_t)-1) return 0;
    return (unsigned char)*a - (unsigned char)*b;
}

char *strcpy(char *dst, const char *src) {
    char *d = dst;
    while ((*d++ = *src++)) {}
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n) {
    char *d = dst;
    while (n && *src) { *d++ = *src++; n--; }
    while (n--) *d++ = 0;
    return dst;
}

char *strchr(const char *s, int c) {
    char ch = (char)c;
    for (;; s++) {
        if (*s == ch) return (char *)s;
        if (!*s) return 0;
    }
}

/* ---- stdlib.h extras ---- */

int abs(int x) {
    return x < 0 ? -x : x;
}

int atoi(const char *s) {
    int sign = 1, v = 0;
    while (*s == ' ' || *s == '\t') s++;
    if (*s == '-') { sign = -1; s++; } else if (*s == '+') s++;
    while (*s >= '0' && *s <= '9') { v = v * 10 + (*s - '0'); s++; }
    return sign * v;
}
