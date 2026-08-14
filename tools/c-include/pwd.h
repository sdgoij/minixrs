/* Minimal pwd.h for the minix OS — no user database. Every process runs
 * as uid 0; the *_r lookups return ENOENT (no such user). */
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

#ifdef __cplusplus
}
#endif

#endif
