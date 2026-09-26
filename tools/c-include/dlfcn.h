/* dlfcn.h — loading a shared object at run time.
 *
 * A dynamically linked program can: the calls below reach the loader through `libc.so`. A
 * statically linked image has no loader in its address space, so there `dlopen` returns
 * null and `dlerror` says why.
 *
 * Binding is eager, so RTLD_LAZY and RTLD_NOW ask the same thing of this loader — it
 * resolves an object completely before `dlopen` returns. RTLD_GLOBAL adds the object's
 * symbols to the global scope; RTLD_LOCAL keeps them reachable through the object's own
 * handle. `dlclose` never unloads: the object stays mapped and loaded, and a second
 * `dlopen` of it hands back the same handle. */
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

/* dladdr — reports "not found". */
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
