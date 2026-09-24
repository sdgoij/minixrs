/* sys/ioctl.h for the minix OS — the device-control request encoding.
 *
 * `ioctl`'s request number is the NetBSD/BSD layout, not Linux's: the
 * direction in the top two bits, the argument's size in bits 16-28, a type
 * letter in bits 8-15, and the request in the low byte. The macros below build
 * the same numbers `net::ioc_encode` does, which is what every driver in the
 * tree and the tty server's own TIOC* constants use. */
#ifndef _SYS_IOCTL_H
#define _SYS_IOCTL_H

#ifdef __cplusplus
extern "C" {
#endif

struct winsize {
    unsigned short ws_row;
    unsigned short ws_col;
    unsigned short ws_xpixel;
    unsigned short ws_ypixel;
};

#define _IOC(dir, type, nr, size)                                 \
    ((unsigned long)(dir) | (((unsigned long)(size) & 0x1fffu) << 16) | \
     ((unsigned long)(type) << 8) | (unsigned long)(nr))

#define _IO(t, nr) _IOC(0x00000000u, t, nr, 0)
#define _IOR(t, nr, s) _IOC(0x40000000u, t, nr, sizeof(s))
#define _IOW(t, nr, s) _IOC(0x80000000u, t, nr, sizeof(s))
#define _IOWR(t, nr, s) _IOC(0xc0000000u, t, nr, sizeof(s))

/* The two every program with a terminal reads: the window size, which the tty
 * server implements for the console lines and the pty masters alike. The same
 * numbers the tty server's TIOCGWINSZ/TIOCSWINSZ constants hold. */
#define TIOCGWINSZ _IOR('t', 104, struct winsize)
#define TIOCSWINSZ _IOW('t', 103, struct winsize)

/* A pointer or an integer, depending on the request; the encoding says which. */
int ioctl(int fd, unsigned long request, void *arg);

#ifdef __cplusplus
}
#endif

#endif
