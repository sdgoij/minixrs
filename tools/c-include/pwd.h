/* pwd.h for the minix OS. Lookups read /etc/passwd; the *_r pair returns
 * ENOENT when no entry matches. */
#ifndef _PWD_H
#define _PWD_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

struct passwd {
    char *pw_name;
    char *pw_passwd;
    uid_t pw_uid;
    gid_t pw_gid;
    char *pw_gecos;
    char *pw_dir;
    char *pw_shell;
};

int getpwnam_r(const char *name, struct passwd *pwd, char *buf, size_t buflen,
               struct passwd **result);
int getpwuid_r(uid_t uid, struct passwd *pwd, char *buf, size_t buflen,
               struct passwd **result);

/* The non-reentrant pair. bash decides these are declared by grepping the
 * preprocessed header for the substring "getpwuid", which `getpwuid_r` above
 * satisfies - so it skips its own extern and needs the real declaration here. */
struct passwd *getpwuid(uid_t uid);
struct passwd *getpwnam(const char *name);

#ifdef __cplusplus
}
#endif

#endif
