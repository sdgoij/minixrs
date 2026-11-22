/* utmpx.h for the minix OS - the login-record file.
 *
 * The entries are `struct utmpx` images stored one after another in
 * `/var/run/utmpx` (crates/minix-std/src/utmpx.rs owns the file and the
 * layout; crates/minix-libc owns the cursor calls). The port does not
 * implement NetBSD's on-disk utmp v2 encoding: the file holds this struct.
 *
 * The socket-address field is spelled out rather than named
 * `struct sockaddr_storage`, because the port's `sys/socket.h` declares that
 * type 2-aligned while this layout has to put `ut_tv` where NetBSD's header
 * does. Only the field's size (128) and alignment (8) matter: nothing the
 * port writes or reads looks inside it.
 */
#ifndef _UTMPX_H_
#define _UTMPX_H_

#include <sys/types.h>
#include <sys/time.h>
#include <stdint.h>

#define _PATH_UTMPX "/var/run/utmpx"

#define _UTX_USERSIZE 32
#define _UTX_LINESIZE 32
#define _UTX_IDSIZE 4
#define _UTX_HOSTSIZE 256

#define EMPTY 0
#define RUN_LVL 1
#define BOOT_TIME 2
#define OLD_TIME 3
#define NEW_TIME 4
#define INIT_PROCESS 5
#define LOGIN_PROCESS 6
#define USER_PROCESS 7
#define DEAD_PROCESS 8

struct utmpx_ss {
    uint64_t __align;
    uint8_t __pad[120];
};

struct utmpx {
    char ut_name[_UTX_USERSIZE]; /* login name */
    char ut_id[_UTX_IDSIZE];     /* inittab id */
    char ut_line[_UTX_LINESIZE]; /* tty name, without /dev/ */
    char ut_host[_UTX_HOSTSIZE]; /* host name */
    uint16_t ut_session;         /* session id used for windowing */
    uint16_t ut_type;            /* type of this entry */
    pid_t ut_pid;                /* process id creating the entry */
    struct {
        uint16_t e_termination; /* process termination signal */
        uint16_t e_exit;        /* process exit status */
    } ut_exit;
    struct utmpx_ss ut_ss; /* address where the entry was made from */
    struct timeval ut_tv;  /* time the entry was created */
    uint8_t ut_pad[36];    /* reserved for future use */
};

#define ut_user ut_name
#define ut_xtime ut_tv.tv_sec

#ifdef __cplusplus
extern "C" {
#endif

void setutxent(void);
void endutxent(void);
struct utmpx *getutxent(void);
int utmpxname(const char *file);

#ifdef __cplusplus
}
#endif

#endif /* !_UTMPX_H_ */
