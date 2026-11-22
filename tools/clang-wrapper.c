/*
 * /usr/bin/clang: the front door to the in-guest compiler driver.
 *
 * The driver itself is a bare-metal clang for <machine>-unknown-none. It knows
 * nothing about this system: no headers, no linker, no layout. Everything that
 * makes it able to build a minix program is added here, and the driver is then
 * exec'd (or, for a link, waited for so the output can be made executable).
 *
 * Why a wrapper and not a `clang.cfg`: the link inputs (crt0, the archives, the
 * linker script, the image bases) must be passed on a *link* line only. A config
 * file cannot express that, so a `-c` compile would be handed object files.
 *
 * Why the link runs as a child: `ld.lld` creates the output with the mode it is
 * given, and a compiled program that is not executable is a wart every caller
 * would have to work around. Nothing in a link needs the exec'd image.
 */

#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

/* The values the build sets (`cargo xtask build llvm-minix`). The defaults keep
 * the file compilable on its own and are the x86_64 ones. */
#ifndef MINIX_DRIVER
#define MINIX_DRIVER "/usr/libexec/clang"
#endif
#ifndef MINIX_INSTALL_DIR
#define MINIX_INSTALL_DIR "/usr/libexec"
#endif
#ifndef MINIX_TARGET
#define MINIX_TARGET "x86_64-unknown-none-elf"
#endif
#ifndef MINIX_RESOURCE
#define MINIX_RESOURCE "/usr/lib/clang/23"
#endif
#ifndef MINIX_IMAGE_BASE
#define MINIX_IMAGE_BASE "0x4000000"
#endif
#ifndef MINIX_LOADER_BASE
#define MINIX_LOADER_BASE "0x8000000"
#endif
#ifndef MINIX_CRT0
#define MINIX_CRT0 "/usr/lib/crt0.o"
#endif
#ifndef MINIX_SCRIPT
#define MINIX_SCRIPT "/usr/lib/minix-user.ld"
#endif
#ifndef MINIX_LIBC
#define MINIX_LIBC "/usr/lib/libc.a"
#endif
#ifndef MINIX_LIBCXX
#define MINIX_LIBCXX "/usr/lib/libstdc++.a"
#endif
#ifndef MINIX_LD
#define MINIX_LD "/usr/bin/ld.lld"
#endif

/* Enough room for the fixed flags of the longest line below. */
#define EXTRA 40

static int ends_with(const char *text, const char *suffix) {
    size_t tl = strlen(text), sl = strlen(suffix);
    return tl >= sl && strcmp(text + tl - sl, suffix) == 0;
}

/* A line that produces no program: nothing to link, nothing to chmod. */
static int compiles_only(int argc, char **argv) {
    static const char *const alone[] = {
        "-c", "-S", "-E", "-fsyntax-only", "-M", "-MM", "-MD", "-MMD",
        "-MG", "-MP", "-MJ", "-###", "--version", "--help", "-dumpmachine",
        "-dumpversion", "-dumpversion", "--print-search-dirs",
    };
    if (argc == 2 && (strcmp(argv[1], "-v") == 0 || strcmp(argv[1], "-V") == 0))
        return 1;
    for (int i = 1; i < argc; i++)
        for (size_t j = 0; j < sizeof alone / sizeof alone[0]; j++)
            if (strcmp(argv[i], alone[j]) == 0)
                return 1;
    for (int i = 1; i < argc; i++)
        if (strncmp(argv[i], "-print-", 7) == 0)
            return 1;
    return 0;
}

/* The value of the last `-o`, or NULL. */
static const char *output_of(int argc, char **argv) {
    const char *out = NULL;
    for (int i = 1; i < argc - 1; i++)
        if (strcmp(argv[i], "-o") == 0)
            out = argv[i + 1];
    return out;
}

int main(int argc, char **argv) {
    char *out[argc + EXTRA];
    int n = 0;
    int cxx = argc > 0 && ends_with(argv[0], "clang++");
    int link = !compiles_only(argc, argv);

    out[n++] = (char *)MINIX_DRIVER;
    /* The `-elf` environment is what selects clang's bare-metal toolchain: its
     * `isX86BareMetal` needs `UnknownOS` *and* environment `elf`, and without it
     * the driver picks `Generic_ELF`, whose linker step is a `gcc` this system
     * does not have. BareMetal's linker step is `ld.lld` itself. */
    out[n++] = (char *)"--target=" MINIX_TARGET;
    /* The driver finds its own executable through `/proc/self/exe` or
     * `realpath(argv[0])`, and a bare-metal binary has neither guarantee that it
     * will be right. `-ccc-install-dir` states it: the driver then runs
     * `<dir>/clang` for its own cc1 job, which is the same file. */
    out[n++] = (char *)"-ccc-install-dir";
    out[n++] = (char *)MINIX_INSTALL_DIR;
    /* The driver's own path for its cc1 job, stated explicitly. With canonical
     * prefixes (the default) clang's driver calls `llvm::sys::fs::getMainExecutable`,
     * and LLVM's Unix implementation answers only for hosts it has a branch for:
     * the `/proc/self/exe` read is under `__linux__`, which this bare-metal triple
     * does not define, and its `dladdr` fallback has no static-executable answer.
     * It therefore returns "" here, the cc1 program name comes out empty, and every
     * external compile fails with ENOENT. `/proc/self/exe` exists in this system
     * (and `/proc/<pid>/exe`), so this is LLVM's probe, not the filesystem's; a
     * driver built with a minix branch would stop needing the flag. */
    out[n++] = (char *)"-no-canonical-prefixes";
    out[n++] = (char *)"-resource-dir";
    out[n++] = (char *)MINIX_RESOURCE;
    /* The driver is a bare-metal triple, so its only default include path is
     * the resource directory. Name all three explicitly, and in this order:
     * libc++'s wrappers `include_next` the C header of the same name. */
    out[n++] = (char *)"-nostdinc";
    /* libc++ before the resource directory for C++: its wrappers (`stddef.h`,
     * `stdint.h`) `include_next` the C header, and libc++'s `<cstddef>` refuses
     * a `<stddef.h>` that came from anywhere else. The resource directory is
     * still searched, after the C++ tree and before the C headers. */
    if (cxx) {
        out[n++] = (char *)"--driver-mode=g++";
        /* The staged runtime is built with LIBCXX_ENABLE_EXCEPTIONS=OFF (there is
         * no unwinder), and libc++'s headers pick their throw path from
         * `__cpp_exceptions`. Compiling a unit with exceptions on leaves their
         * inline throw helpers referencing `__cxa_allocate_exception` and
         * `__cxa_throw`, which the archive does not carry. */
        out[n++] = (char *)"-fno-exceptions";
        out[n++] = (char *)"-nostdinc++";
        out[n++] = (char *)"-isystem";
        out[n++] = (char *)"/usr/include/c++/v1";
        /* The staged libc++ was configured with these two; a caller that does
         * not repeat them gets a different rune table and is not ABI-visible,
         * but the definitions cost nothing and keep the guest and the build
         * root on one configuration. */
        out[n++] = (char *)"-D_LIBCPP_PROVIDES_DEFAULT_RUNE_TABLE";
        out[n++] = (char *)"-D_LIBCPP_NO_ABI_TAG";
    }
    out[n++] = (char *)"-isystem";
    out[n++] = (char *)MINIX_RESOURCE "/include";
    out[n++] = (char *)"-isystem";
    out[n++] = (char *)"/usr/include";

    if (link) {
        char image_base[] = "-Wl,--image-base=" MINIX_IMAGE_BASE;
        char base_address[] = "-Wl,--defsym=BASE_ADDRESS=" MINIX_IMAGE_BASE;
        char loader_base[] = "-Wl,--defsym=LOADER_BASE=" MINIX_LOADER_BASE;
        /* `--ld-path` rather than `-fuse-ld=lld`: the driver would otherwise
         * search PATH, and the toolchain has to work from a shell whose PATH
         * is whatever `init` gave it. */
        out[n++] = (char *)"--ld-path=" MINIX_LD;
        out[n++] = (char *)"-nostdlib";
        out[n++] = (char *)"-nostartfiles";
        out[n++] = (char *)MINIX_CRT0;
        for (int i = 1; i < argc; i++)
            out[n++] = argv[i];
        out[n++] = image_base;
        out[n++] = base_address;
        out[n++] = loader_base;
        out[n++] = (char *)"-Wl,-T," MINIX_SCRIPT;
        out[n++] = (char *)"-Wl,-z,norelro";
        out[n++] = (char *)"-Wl,--no-eh-frame-hdr";
        /* Minix has one C library: `librt` and `libpthread` are parts of
         * `libc.a`. Members of the C++ runtime carry `.deplibs` autolink
         * entries naming them, and LLD stops the link when it cannot find a
         * separate library by that name, so dependent libraries are ignored.
         * Everything a program needs is `libc.a`/`libstdc++.a`, both named
         * below. */
        out[n++] = (char *)"-Wl,--no-dependent-libraries";
        out[n++] = (char *)MINIX_LIBC;
        if (cxx)
            out[n++] = (char *)MINIX_LIBCXX;
    } else {
        for (int i = 1; i < argc; i++)
            out[n++] = argv[i];
    }
    out[n] = NULL;

    if (!link) {
        execv(MINIX_DRIVER, out);
        perror("clang");
        return 127;
    }

    const char *target = output_of(argc, argv);
    pid_t pid = fork();
    if (pid == 0) {
        execv(MINIX_DRIVER, out);
        perror("clang");
        _exit(127);
    }
    if (pid < 0) {
        perror("clang: fork");
        return 1;
    }
    int status = 0;
    if (waitpid(pid, &status, 0) < 0) {
        perror("clang: waitpid");
        return 1;
    }
    if (!WIFEXITED(status)) {
        fprintf(stderr, "clang: the linker driver died\n");
        return 1;
    }
    if (WEXITSTATUS(status) != 0)
        return WEXITSTATUS(status);
    /* A linker does not mark its output executable; a compiler does. */
    if (target != NULL && chmod(target, 0777) != 0)
        perror(target);
    return 0;
}
