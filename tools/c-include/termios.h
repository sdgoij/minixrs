/* termios.h for the minix OS.
 *
 * The kernel has this: the tty server (crates/servers/src/tty.rs) implements
 * the NetBSD TIOCGETA/TIOCSETA family and minix-std owns the request codes.
 * The flag values and the struct layout below are the server's — the ioctl
 * request numbers are derived from sizeof(struct termios), so the struct
 * cannot change shape without the codes changing with it.
 *
 * tcgetattr/tcsetattr are real here: readline needs them to leave canonical
 * mode for line editing. */
#ifndef _TERMIOS_H
#define _TERMIOS_H

#ifdef __cplusplus
extern "C" {
#endif

typedef unsigned char cc_t;
typedef unsigned int speed_t;
typedef unsigned int tcflag_t;

#define NCCS 20

struct termios {
    tcflag_t c_iflag;      /* input mode flags */
    tcflag_t c_oflag;      /* output mode flags */
    tcflag_t c_cflag;      /* control mode flags */
    tcflag_t c_lflag;      /* local mode flags */
    cc_t c_cc[NCCS];       /* control characters */
    speed_t c_ispeed;      /* input speed */
    speed_t c_ospeed;      /* output speed */
};

/* Control-character subscripts (c_cc). */
#define VEOF 0
#define VEOL 1
#define VEOL2 2
#define VERASE 3
#define VWERASE 4
#define VKILL 5
#define VREPRINT 6
#define VINTR 8
#define VQUIT 9
#define VSUSP 10
#define VDSUSP 11
#define VSTART 12
#define VSTOP 13
#define VLNEXT 14
#define VDISCARD 15
#define VMIN 16
#define VTIME 17
#define VSTATUS 18

/* c_iflag */
#define IGNBRK 0x00000001
#define BRKINT 0x00000002
#define IGNPAR 0x00000004
#define PARMRK 0x00000008
#define INPCK 0x00000010
#define ISTRIP 0x00000020
#define INLCR 0x00000040
#define IGNCR 0x00000080
#define ICRNL 0x00000100
#define IXON 0x00000200
#define IXOFF 0x00000400
#define IXANY 0x00000800
#define IMAXBEL 0x00002000

/* c_oflag */
#define OPOST 0x00000001
#define ONLCR 0x00000002
#define OXTABS 0x00000004
#define ONOEOT 0x00000008
#define OCRNL 0x00000010
#define ONOCR 0x00000020
#define ONLRET 0x00000040

/* c_cflag */
#define CIGNORE 0x00000001
#define CSIZE 0x00000300
#define CS5 0x00000000
#define CS6 0x00000100
#define CS7 0x00000200
#define CS8 0x00000300
#define CSTOPB 0x00000400
#define CREAD 0x00000800
#define PARENB 0x00001000
#define PARODD 0x00002000
#define HUPCL 0x00004000
#define CLOCAL 0x00008000
#define CRTSCTS 0x00010000
#define CDTRCTS 0x00020000
#define MDMBUF 0x00100000

/* c_lflag */
#define ECHOKE 0x00000001
#define ECHOE 0x00000002
#define ECHOK 0x00000004
#define ECHO 0x00000008
#define ECHONL 0x00000010
#define ECHOPRT 0x00000020
#define ECHOCTL 0x00000040
#define ISIG 0x00000080
#define ICANON 0x00000100
#define ALTWERASE 0x00000200
#define IEXTEN 0x00000400
#define EXTPROC 0x00000800
#define TOSTOP 0x00400000
#define FLUSHO 0x00800000
#define NOKERNINFO 0x02000000
#define PENDIN 0x20000000
#define NOFLSH 0x80000000

/* Baud rates. The tty server keeps the rate as a plain number in c_ispeed and
 * c_ospeed rather than as bits in c_cflag, so these are values, not a mask. */
#define B0 0
#define B50 50
#define B75 75
#define B110 110
#define B134 134
#define B150 150
#define B200 200
#define B300 300
#define B600 600
#define B1200 1200
#define B1800 1800
#define B2400 2400
#define B4800 4800
#define B7200 7200
#define B9600 9600
#define B19200 19200
#define B38400 38400
#define B115200 115200

/* tcsetattr actions. */
#define TCSANOW 0
#define TCSADRAIN 1
#define TCSAFLUSH 2

/* tcflush queues. */
#define TCIFLUSH 1
#define TCOFLUSH 2
#define TCIOFLUSH 3

/* tcflow actions. */
#define TCOOFF 0
#define TCOON 1
#define TCIOFF 2
#define TCION 3

int tcgetattr(int fd, struct termios *t);
int tcsetattr(int fd, int actions, const struct termios *t);
int tcdrain(int fd);
int tcflush(int fd, int queue);
int tcflow(int fd, int action);
int tcgetpgrp(int fd);
int tcsetpgrp(int fd, int pgrp);
int cfgetospeed(const struct termios *t);
int cfgetispeed(const struct termios *t);
int cfsetospeed(struct termios *t, int speed);
int cfsetispeed(struct termios *t, int speed);
void cfmakeraw(struct termios *t);

#ifdef __cplusplus
}
#endif

#endif
