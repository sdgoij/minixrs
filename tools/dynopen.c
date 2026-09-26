/* The `dlopen` consumer: a program that asks the loader for a shared object at run time.
 *
 * `dlopen`, `dlsym`, `dlerror` and `dlclose` come from `libc.so`, which reaches the loader
 * through the `__rtld_*` names the loader answers for it
 * (`crates/ldso/src/rtld.rs::loader_defined`). This program's own link therefore names
 * nothing but libc, and what is under test is the loader's `dlopen` rather than a build-time
 * dependency.
 *
 * Its argument picks the case, one per run, so each of the harness's steps is a separate
 * process and a failure names itself:
 *
 *   load     `/lib/libdlopen.so` is loaded and its two symbols resolved through the handle:
 *            the number printed is that object's code and the text its read-only data. The
 *            same run asserts the negative half of `RTLD_LOCAL` — a global lookup must not
 *            find it.
 *   global   the same object loaded with `RTLD_GLOBAL`, then looked up through
 *            `dlsym(RTLD_DEFAULT, …)`, i.e. the global scope and *not* the handle.
 *   missing  a `dlopen` of a path that does not exist fails, and `dlerror` says so.
 *   tls      a `dlopen` of an object with a thread-local is refused — the loader places one
 *            TLS module, at startup — and `dlerror` names that limit.
 *   close    `dlclose` accepts the handle and the object stays usable: nothing is unloaded.
 *
 * One line per run, because the smoke harness matches whole lines anywhere in the log: a
 * second expected line could be satisfied by an earlier run's output.
 *
 * Freestanding, like `tools/hello.c`: the declarations below must match `minix-libc`, and
 * they are spelled out rather than included so that what this program calls is what the
 * lines below say it calls.
 */

typedef int c_int;
typedef unsigned long size_t;
typedef long ssize_t;

extern ssize_t write(c_int fd, const void *buf, size_t count);
extern void *dlopen(const char *filename, c_int flags);
extern void *dlsym(void *handle, const char *symbol);
extern char *dlerror(void);
extern c_int dlclose(void *handle);

#define RTLD_NOW 0x00002
#define RTLD_GLOBAL 0x00100
#define RTLD_DEFAULT ((void *)0)

#define LIB "/lib/libdlopen.so"
#define TLS_LIB "/lib/libtls1.so"

typedef const char *(*text_fn)(void);
typedef c_int (*marker_fn)(void);

static size_t slen(const char *s) {
    size_t n = 0;
    while (s[n] != 0) {
        n++;
    }
    return n;
}

static c_int same(const char *a, const char *b) {
    while (*a != 0 && *a == *b) {
        a++;
        b++;
    }
    return *a == *b;
}

static void out(const char *s) {
    write(1, s, slen(s));
}

static void num(c_int v) {
    char buf[12];
    c_int i = 12;
    unsigned long u = v < 0 ? (unsigned long)(-(long)v) : (unsigned long)v;
    do {
        buf[--i] = (char)('0' + (u % 10));
        u /= 10;
    } while (u != 0);
    write(1, &buf[i], (size_t)(12 - i));
}

/* The two symbols the object has, read through `h`: the marker from its code and the text
 * from its read-only data. Both are printed as the object states them — this program holds
 * neither, which is what the gate's negative check on `dynopen-text` is about. */
static void print_symbols(void *h) {
    text_fn text = (text_fn)dlsym(h, "dynopen_text");
    marker_fn marker = (marker_fn)dlsym(h, "dynopen_marker");
    if (text == (text_fn)0 || marker == (marker_fn)0) {
        out("dlsym-null");
        return;
    }
    num(marker());
    out(" ");
    out(text());
}

static c_int load_failed(void) {
    out("null=1 msg=");
    /* The loader's message ends its own line. */
    out(dlerror());
    out("\n");
    return 1;
}

int main(c_int argc, char **argv) {
    const char *mode = argc > 1 ? argv[1] : "load";
    void *h;

    if (same(mode, "missing") || same(mode, "tls")) {
        const char *path = same(mode, "tls") ? TLS_LIB : "/no/such-object.so";
        out(same(mode, "tls") ? "dynopen-tls " : "dynopen-missing ");
        h = dlopen(path, RTLD_NOW);
        if (h != RTLD_DEFAULT) {
            out("loaded=1\n");
            return 1;
        }
        return load_failed();
    }

    if (same(mode, "close")) {
        h = dlopen(LIB, RTLD_NOW);
        if (h == RTLD_DEFAULT) {
            out("dynopen-null ");
            return load_failed();
        }
        out("dynopen-close ");
        num(dlclose(h));
        out(" ");
        print_symbols(h);
        out("\n");
        return 0;
    }

    if (same(mode, "global")) {
        h = dlopen(LIB, RTLD_NOW | RTLD_GLOBAL);
        if (h == RTLD_DEFAULT) {
            out("dynopen-null ");
            return load_failed();
        }
        text_fn text = (text_fn)dlsym(RTLD_DEFAULT, "dynopen_text");
        marker_fn marker = (marker_fn)dlsym(RTLD_DEFAULT, "dynopen_marker");
        out("dynopen-global ");
        if (text == (text_fn)0 || marker == (marker_fn)0) {
            out("scope-null\n");
            return 1;
        }
        num(marker());
        out(" ");
        out(text());
        out("\n");
        return 0;
    }

    /* `load`: `RTLD_NOW` without `RTLD_GLOBAL`, so the object is `RTLD_LOCAL`. */
    h = dlopen(LIB, RTLD_NOW);
    if (h == RTLD_DEFAULT) {
        out("dynopen-null ");
        return load_failed();
    }
    out("dynopen-ok ");
    print_symbols(h);
    out(" global-miss=");
    num(dlsym(RTLD_DEFAULT, "dynopen_text") == RTLD_DEFAULT);
    out("\n");
    return 0;
}
