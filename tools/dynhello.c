/* The Phase 0 dynamic executable.
 *
 * A non-PIE `ET_EXEC` linked with `PT_INTERP` (`/libexec/ld.so`) and
 * `DT_NEEDED` (`libdyn.so`): its call to `dyn_message` goes through a PLT/GOT
 * that the interpreter must resolve before `main` runs. It prints what only the
 * shared object contains.
 *
 * Freestanding, like `tools/hello.c`: the syscall surface (`write`/`exit`) comes
 * from `minix-libc`, resolved at link time.
 */

typedef unsigned long size_t;
typedef long ssize_t;

extern ssize_t write(int fd, const void *buf, size_t count);
extern void exit(int status);
extern const char *dyn_message(void);

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;
    const char *m = dyn_message();
    size_t n = 0;
    while (m[n] != 0) {
        n++;
    }
    write(1, m, n);
    write(1, "\n", 1);
    exit(0);
    return 0; /* not reached */
}
