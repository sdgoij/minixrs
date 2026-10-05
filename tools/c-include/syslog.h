/* syslog.h for the minix OS — the priorities and `syslog()` that Mesa's log.c
 * routes to under DETECT_OS_POSIX. There is no syslog service; the message goes
 * to stderr. */
#ifndef _SYSLOG_H
#define _SYSLOG_H

#ifdef __cplusplus
extern "C" {
#endif

#define LOG_EMERG 0
#define LOG_ALERT 1
#define LOG_CRIT 2
#define LOG_ERR 3
#define LOG_WARNING 4
#define LOG_NOTICE 5
#define LOG_INFO 6
#define LOG_DEBUG 7

/* `openlog()` options. */
#define LOG_PID 0x01
#define LOG_NDELAY 0x08

/* Facilities (the high bits of the priority). */
#define LOG_KERN (0 << 3)
#define LOG_USER (1 << 3)
#define LOG_MAIL (2 << 3)
#define LOG_DAEMON (3 << 3)
#define LOG_AUTH (4 << 3)
#define LOG_SYSLOG (5 << 3)
#define LOG_LOCAL0 (16 << 3)

/* `openlog` is accepted and ignored (there is no syslog service); `syslog`
 * itself writes to stderr. */
void openlog(const char *ident, int option, int facility);

void syslog(int priority, const char *format, ...);

#ifdef __cplusplus
}
#endif

#endif
