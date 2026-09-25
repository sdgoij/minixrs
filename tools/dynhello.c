/* The dynamic executable.
 *
 * A non-PIE `ET_EXEC` linked with `PT_INTERP` (`/libexec/ld.so`) and `DT_NEEDED`
 * (`libdyn.so`, `libdyn2.so`): its calls to `dyn_message` and `dyn_second` go
 * through a PLT/GOT the loader must resolve before `main` runs, and `dyn_pointer`
 * is a variable it reads out of libdyn — a COPY, which the loader fills with the
 * object's bytes after applying the object's own `RELATIVE` fixup.
 *
 * It prints one line holding all three values. Each comes from a shared object and
 * none is in this file, so the line is the loader's work — and one line rather
 * than three because the smoke harness matches whole lines anywhere in the log: a
 * second expected line would be satisfied by the first run's output and prove
 * nothing.
 *
 * Its argument asks for a lifecycle to be exercised, and prefixes the line with
 * what happened, which is what makes each of the harness's steps distinct:
 *
 *   (none)    the line as above
 *   fork      the line, prefixed by whether a forked child could call into both
 *             objects and exit cleanly — fork after a dynamic load
 *   exec      replaces itself with `/bin/dynhello re-exec` — exec after one
 *   re-exec   the line, prefixed `re-exec-ok `, printed by the new image
 *
 * Freestanding, like `tools/hello.c`: the syscall surface comes from
 * `minix-libc`, resolved at link time. The declarations below must match it.
 */

typedef unsigned long size_t;
typedef long ssize_t;
typedef int pid_t;

extern ssize_t write(int fd, const void *buf, size_t count);
extern void exit(int status);
extern pid_t fork(void);
extern pid_t wait(int *status);
extern int execv(const char *path, char *const argv[]);
extern const char *dyn_message(void);
extern const char *dyn_second(void);
extern const char *const dyn_pointer;

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

static int streq(const char *a, const char *b) {
    while (*a != 0 && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

/* Whether both objects answer with a non-empty string. The *values* cannot be
 * compared here — the expected strings are deliberately not in this file, which is
 * what makes the gate's line evidence about the loader — so this is what a forked
 * child can check about them. */
static int objects_usable(void) {
    const char *a = dyn_message();
    const char *b = dyn_pointer;
    const char *c = dyn_second();
    return a != 0 && a[0] != 0 && b != 0 && b[0] != 0 && c != 0 && c[0] != 0;
}

int main(int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : 0;

    if (mode != 0 && streq(mode, "fork")) {
        const pid_t pid = fork();
        if (pid < 0) {
            emit("fork-failed ");
        } else if (pid == 0) {
            exit(objects_usable() ? 0 : 1);
        } else {
            int status = 0;
            wait(&status);
            /* The child inherits this process's mappings, so a child that called
             * into both objects and exited 0 is the evidence fork kept them. */
            emit(status == 0 ? "child-ok " : "child-failed ");
        }
    } else if (mode != 0 && streq(mode, "exec")) {
        char *const again[] = {"/bin/dynhello", "re-exec", 0};
        execv("/bin/dynhello", again);
        emit("exec-failed ");
    } else if (mode != 0 && streq(mode, "re-exec")) {
        emit("re-exec-ok ");
    }

    emit(dyn_message());
    write(1, " ", 1);
    emit(dyn_pointer);
    write(1, " ", 1);
    emit(dyn_second());
    write(1, "\n", 1);
    exit(0);
    return 0; /* not reached */
}
