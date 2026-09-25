/* The dynamic executable.
 *
 * A non-PIE `ET_EXEC` linked with `PT_INTERP` (`/libexec/ld.so`) and `DT_NEEDED`
 * (`libdyn.so`, `libdyn2.so`): its calls to `dyn_message`, `dyn_data` and
 * `dyn_second` go through a PLT/GOT the loader must resolve before `main` runs,
 * and `dyn_data`'s result is only a usable pointer once the loader has applied
 * libdyn's own `RELATIVE` fixup.
 *
 * It prints one line holding all three values. Each comes from a shared object and
 * none is in this file, so the line is the loader's work — and one line rather
 * than three because the smoke harness matches whole lines anywhere in the log: a
 * second expected line would be satisfied by the first run's output and prove
 * nothing.
 *
 * Freestanding, like `tools/hello.c`: the syscall surface (`write`/`exit`) comes
 * from `minix-libc`, resolved at link time.
 */

typedef unsigned long size_t;
typedef long ssize_t;

extern ssize_t write(int fd, const void *buf, size_t count);
extern void exit(int status);
extern const char *dyn_message(void);
extern const char *dyn_data(void);
extern const char *dyn_second(void);

static size_t slen(const char *s) {
    size_t n = 0;
    while (s[n] != 0) {
        n++;
    }
    return n;
}

static void emit(const char *s) {
    write(1, s, slen(s));
}

int main(int argc, char **argv) {
    (void)argc;
    (void)argv;
    emit(dyn_message());
    write(1, " ", 1);
    emit(dyn_data());
    write(1, " ", 1);
    emit(dyn_second());
    write(1, "\n", 1);
    exit(0);
    return 0; /* not reached */
}
