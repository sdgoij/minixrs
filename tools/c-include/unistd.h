/* Minimal unistd.h for the minix OS. */
#ifndef _UNISTD_H
#define _UNISTD_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define STDIN_FILENO 0
#define STDOUT_FILENO 1
#define STDERR_FILENO 2

ssize_t read(int fd, void *buf, size_t count);
ssize_t write(int fd, const void *buf, size_t count);
int close(int fd);
int getpid(void);
int getppid(void);
int isatty(int fd);
int gethostname(char *name, size_t len);
int getsid(int pid);

int dup(int fd);
int dup2(int fd, int newfd);
int pipe(int fds[2]);
off_t lseek(int fd, off_t offset, int whence);
unsigned int alarm(unsigned int seconds);
int unlink(const char *path);

pid_t fork(void);
int execve(const char *path, char *const argv[], char *const envp[]);
int execv(const char *path, char *const argv[]);
pid_t setsid(void);
void _exit(int status);
char *getcwd(char *buf, size_t size);
int chdir(const char *path);
int fchdir(int fd);
int link(const char *oldpath, const char *newpath);
int symlink(const char *target, const char *linkpath);
ssize_t readlink(const char *path, char *buf, size_t bufsiz);
int ftruncate(int fd, off_t length);
int access(const char *path, int mode);
int fchown(int fd, uid_t owner, gid_t group);
int usleep(unsigned int usec);

#define F_OK 0
#define X_OK 1
#define W_OK 2
#define R_OK 4

#define PATH_MAX 4096

/* The process environment, which this port publishes from the `envp` the kernel
 * hands `main` (`__minix_set_environ`, called by crt0). POSIX declares it here,
 * though glibc also puts it in stdlib.h. */
extern char **environ;

/* POSIX.1-2008. Nothing in this port gates on it, but C code does: bash only
 * uses an `int` wait status instead of `union wait` when _POSIX_VERSION is
 * defined (include/posixwait.h), and an `int` is what PM's wait statuses are.
 * Declaring it is a claim the port has the interfaces POSIX names; anything it
 * does not have is a gap to fill, not a reason to hide behind the absence. */
#define _POSIX_VERSION 200809L

#define _SC_ARG_MAX 0
#define _SC_CHILD_MAX 1
#define _SC_CLK_TCK 2
#define _SC_NGROUPS_MAX 3
#define _SC_OPEN_MAX 4
#define _SC_STREAM_MAX 5
#define _SC_TZNAME_MAX 6
#define _SC_JOB_CONTROL 7
#define _SC_SAVED_IDS 8
#define _SC_REALTIME_SIGNALS 9
#define _SC_PRIORITY_SCHEDULING 10
#define _SC_PAGESIZE 30
#define _SC_PAGE_SIZE 30
#define _SC_GETPW_R_SIZE_MAX 69

#define _POSIX_ARG_MAX 4096
#define _POSIX_OPEN_MAX 32
#define _POSIX_CHILD_MAX 16

long sysconf(int name);
int getpagesize(void);
uid_t getuid(void);

/* Process credentials. bash calls all of these unconditionally. */
uid_t geteuid(void);
gid_t getgid(void);
gid_t getegid(void);
int setuid(uid_t uid);
int setgid(gid_t gid);
int seteuid(uid_t uid);
int setegid(gid_t gid);
int getgroups(int size, gid_t list[]);
/* POSIX files setgroups() under <grp.h>, which this port does not ship yet;
 * it is declared here so its export has a header. */
int setgroups(size_t size, const gid_t *list);
/* BSD's "did this program gain privilege?", which this port answers from the
 * credentials PM reports. */
int issetugid(void);

unsigned int sleep(unsigned int seconds);
char *ttyname(int fd);

#ifdef __cplusplus
}
#endif

#endif
