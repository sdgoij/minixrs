/* Minimal dlfcn.h for the minix OS — dynamic loading is not supported
 * (statically linked image); the functions are stubs. */
#ifndef _DLFCN_H
#define _DLFCN_H

#define RTLD_LAZY 0x00001
#define RTLD_NOW 0x00002
#define RTLD_GLOBAL 0x00100
#define RTLD_LOCAL 0x00000

#ifdef __cplusplus
extern "C" {
#endif

void *dlopen(const char *filename, int flags);
void *dlsym(void *handle, const char *symbol);
int dlclose(void *handle);
char *dlerror(void);

/* dladdr — reports "not found" (statically linked image). */
typedef struct {
    const char *dli_fname;
    void *dli_fbase;
    const char *dli_sname;
    void *dli_saddr;
} Dl_info;

int dladdr(void *addr, Dl_info *info);

#ifdef __cplusplus
}
#endif

#endif
