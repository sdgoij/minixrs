/* The dynamically linked C program: linked against `libc.so`, not the static
 * library.
 *
 * Every function it calls — `open`, `printf`, `strerror`, `__errno_location`,
 * and the `exit` its `crt0` ends on — is a symbol `libc.so` has to define and
 * the loader has to resolve before `main` runs. None of them is in this file.
 *
 * Two of them are more than code, and they are why the line below is evidence
 * of the whole chain rather than of a single call:
 *
 *   - `errno` is a `#[thread_local]` in `libc.so`, so reading it goes through
 *     the `__tls_get_addr` the loader supplies, against a block the loader laid
 *     out from that object's own `PT_TLS` and installed before anything ran;
 *   - `strerror` returns a pointer into a buffer inside `libc.so`, so the message
 *     printed here is that object's data, reached through a relocation the loader
 *     applied.
 *
 * The `ctor` field is this program's *own* constructor, and it is the third
 * thing the line covers: `crt0` walks this image's `.init_array` itself, which is
 * what a dynamically linked program used to lose — the call into the C library
 * resolved into the shared object, whose array is the object's own and empty.
 * The field is 0 unless a constructor ran, so it fails on its own if that walk
 * goes missing again. `volatile` because `-O2` otherwise folds away a static only
 * a constructor writes.
 *
 * The message is therefore deliberately *not* in this file, and
 * `just test-dynlink-x86` fails the gate if the executable contains it.
 *
 * It is not a test binary: `/bin/dynclib` is in
 * `crates/boot-image/src/manifest.rs`'s `BOOT_BINS`, so every image carries it (with
 * `/lib/libc.so` and the loader it needs). This gate is where it runs.
 *
 * `hold` exists for `tools/dso_share_probe.py`, which measures whether two
 * processes mapping this object share its frames (Phase 5). It prints a marker and
 * then blocks on the console, so the two lives the probe needs can be started from
 * the shell — as a pipeline, `/bin/dynclib hold | /bin/dynclib hold` — which is the
 * shell's own fork and exec, the path already exercised by every other command.
 * (A `fork`+`exec` inside this program was tried first and hung in the exec; see
 * the probe's notes. Nothing here needs it.)
 *
 * The frames themselves are not read here: only the kernel knows a virtual
 * address's frame, and asking it from inside the process under test would be the
 * subject measuring itself.
 *
 * Freestanding, like `tools/hello.c`: the declarations below must match
 * `minix-libc`.
 */

typedef int c_int;

extern int printf(const char *fmt, ...);
extern c_int open(const char *path, c_int flags, c_int mode);
extern char *strerror(c_int errnum);
extern c_int *__errno_location(void);
extern long read(c_int fd, void *buf, unsigned long count);
extern long write(c_int fd, const void *buf, unsigned long count);
extern void exit(c_int status);

static volatile c_int ctor_ran;

__attribute__((constructor)) static void program_ctor(void) {
    ctor_ran = 1;
}

static int same(const char *a, const char *b) {
    while (*a != 0 && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

/* Block on the console. A read that returns nothing is still a return, so the
 * process leaves only when the console is closed — which is what the probe does
 * by stopping QEMU. */
static void wait_for_console(void) {
    char c;
    while (read(0, &c, 1) == 1) {
    }
}

static void hold_mode(void) {
    /* On stderr, which a pipeline does not redirect: the first side of
     * `/bin/dynclib hold | /bin/dynclib hold` has its stdout in the pipe, and the
     * probe needs to see *both* lives reach `main`. */
    write(2, "dynclib-hold\n", 13);
    wait_for_console();
    exit(0);
}

int main(c_int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : 0;
    if (mode != 0 && same(mode, "hold")) {
        hold_mode();
    }

    /* A path that cannot exist, so the failure is what sets `errno` — reading it
     * afterwards is what exercises the loader's thread-local storage. Opening
     * `/dev/null` in the same program would prove nothing about either. */
    if (open("/no-such-file", 0, 0) >= 0) {
        printf("libc-dyn-bad open succeeded\n");
        return 1;
    }
    c_int e = *__errno_location();
    printf("libc-dyn-ok errno=%d msg=%s ctor=%d\n", e, strerror(e), ctor_ran);
    return 0;
}
