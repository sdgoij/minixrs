/* grp.h for the minix OS — the group-entry lookups.
 *
 * The port reads /etc/group (crates/minix-std/src/group.rs parses it, and
 * crates/minix-libc answers these four calls). `gr_mem` is the
 * NULL-terminated pointer array POSIX requires: it is laid out after every
 * string in the caller's buffer, so the `_r` pair's size covers names *and*
 * pointers. */
#ifndef _GRP_H
#define _GRP_H

#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

struct group {
    char *gr_name;
    char *gr_passwd;
    gid_t gr_gid;
    char **gr_mem;
};

int getgrnam_r(const char *name, struct group *grp, char *buf, size_t buflen,
               struct group **result);
int getgrgid_r(gid_t gid, struct group *grp, char *buf, size_t buflen,
               struct group **result);
struct group *getgrnam(const char *name);
struct group *getgrgid(gid_t gid);
/* Walk /etc/group in file order. A consumer that sees grp.h assumes these
 * exist (bash's group-name completion does), so they are part of the header
 * rather than optional. */
struct group *getgrent(void);
void setgrent(void);
void endgrent(void);
int getgrouplist(const char *user, gid_t group, gid_t *groups, int *ngroups);

#ifdef __cplusplus
}
#endif

#endif
