/* Minimal dirent.h for the minix OS — VFS getdents-backed directory
 * streams. */
#ifndef _DIRENT_H
#define _DIRENT_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct DIR DIR;

struct dirent {
    ino_t d_ino;
    off_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[256];
};

#define DT_UNKNOWN 0
#define DT_REG 8
#define DT_DIR 4
#define DT_CHR 2
#define DT_BLK 6
#define DT_FIFO 1
#define DT_LNK 10
#define DT_SOCK 12

DIR *opendir(const char *name);
struct dirent *readdir(DIR *dirp);
int closedir(DIR *dirp);
void rewinddir(DIR *dirp);

/* scandir/alphasort: the whole directory as a malloc'd array of entry copies
 * (readdir's entry is reused, so a copy is required), filtered and sorted. */
int scandir(const char *dirp, struct dirent ***namelist,
            int (*filter)(const struct dirent *),
            int (*compar)(const struct dirent **, const struct dirent **));
int alphasort(const struct dirent **a, const struct dirent **b);

#ifdef __cplusplus
}
#endif

#endif
