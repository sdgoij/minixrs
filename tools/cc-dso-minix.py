#!/usr/bin/env python3
"""A `cc` for C projects that build *shared objects* for minix.

`tools/cc-minix.py` links an executable against the static `minix-libc` rlib,
which is what bash and the smoke binaries want. Mesa and `libdrm` are the other
shape: their build (meson) compiles many C files with one `cc`, links some of
them into a `.so` that names `libc.so`, and links a program against that. A
static-rlib link cannot produce either, so this is a second `cc` rather than a
flag on the first.

It is the `cc` half of §6.10 stage 3c-0. The pieces it reuses are the ones every
minix C build already shares:

  * **compile** — clang for the arch's machine, `-fPIC` (a shared object is
    position-independent; `tools/cc-minix.py`'s `-fno-pic` is for executables),
    with `tools/ccflags.py`'s hermetic include set, so a missing header is an
    error rather than the host's glibc.
  * **link** — LLD directly (`tools/lld.py`), the same `-z norelro` a shared
    object takes so it costs three VM regions and not four, against the port's
    `libc.so` (`target/<triple>/release/libc.so`, the object `just dynlib-<arch>`
    publishes). A program links with `tools/crt0-<arch>.S` and the loader's
    `PT_INTERP` (`/libexec/ld.so`), the same two things `tools/cdyn.py` supplies
    through rustc — here without rustc, because a build system calls this for
    every link.

Point a build at it by name, naming the target first where it is not x86_64:

    CC="python3 <repo>/tools/cc-dso-minix.py"
    CC="python3 <repo>/tools/cc-dso-minix.py x86"

Usage: python tools/cc-dso-minix.py [arch] <cc arguments...>
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import Arch, resolve_argv  # noqa: E402
from ccflags import compile_flags  # noqa: E402
from lld import NO_RELRO, find_lld  # noqa: E402

LD_SCRIPT = ROOT / "tools" / "minix-user.ld"
INTERP = "/libexec/ld.so"
LIBC = "libc.so"
WORK_ROOT = ROOT / "target" / "cc-dso"

# The port's C++ runtime, built by `just libcxx-x86` (libc++ + libc++abi merged
# into one archive). C++ compiles need the headers; every link gets the archive
# so a C++ object's `std::` references resolve (an unreferenced archive member
# costs nothing).
LIBCXX_INCLUDE = ROOT / "rust" / "src" / "llvm-project" / "libcxx" / "include"
LIBCXXABI_INCLUDE = ROOT / "rust" / "src" / "llvm-project" / "libcxxabi" / "include"
LIBCXX_CONFIG_INCLUDE = ROOT / "target" / "cxx" / "libcxx-build" / "include" / "c++" / "v1"
LIBCXX_RUNTIME = ROOT / "target" / "cxx" / "minix-runtime" / "libstdc++.a"

# A `cc` line is two commands in one, so every argument has to be classified: a
# compile-class flag (`-I`, `-O2`, `-fPIC`) must not reach the linker, and a
# link-class one (`-Wl,…`, `-L`, `-l`) must not reach clang. The two value sets
# are disjoint, which is what lets one pass decide.
COMPILE_VALUE_FLAGS = frozenset({
    "-I", "-D", "-U", "-isystem", "-include", "-imacros", "-MF", "-MT", "-MQ",
    "-x", "--param", "-std", "-Xclang", "-idirafter", "-iquote", "-isysroot",
    "--sysroot",
})
LINK_VALUE_FLAGS = frozenset({"-L", "-l", "-Xlinker", "-u", "-z", "-T"})
# Driver-level link flags lld does not accept as spelled; `-shared` is added
# explicitly and the rest are dropped or translated in [`link_args`].
LINK_FLAGS = frozenset({"-shared", "-static", "-rdynamic", "-pthread", "-nostdlib",
                        "-nodefaultlibs", "-s"})
COMPILE_ONLY = frozenset({"-c", "-E", "-S", "-M", "-MM", "-fsyntax-only"})
# `-Wl,` options whose value meson passes as the *next* token rather than
# comma-joined (the version script and dynamic list are build artifacts kept as
# separate arguments). Without this the file is classified as an input and
# compiled.
WL_OPT_TAKES_VALUE = frozenset({
    "--version-script", "--dynamic-list", "--script", "-T", "-Map",
    "--retain-symbols-file", "--defsym", "-e", "--dynamic-linker",
    "--just-symbols",
})
# Libraries meson's platform probes picked up from the *host* (or that live in the
# port's libc.so): the port provides libc/libm/pthread/dl/rt itself.
SPURIOUS_LIBS = frozenset({
    "-lm", "-lpthread", "-ldl", "-lrt", "-lc", "-lws2_32", "-lgcc", "-lgcc_s",
})
VERSION_PROBES = frozenset({
    "--version", "-v", "-V", "-qversion", "-dumpversion", "-dumpmachine", "--help",
})


def run(cmd: list[object]) -> int:
    print("+", " ".join(str(c) for c in cmd), file=sys.stderr)
    return subprocess.run([str(c) for c in cmd]).returncode


def pic_cflags(arch: Arch) -> list[str]:
    """`arch.base_cflags()` with `-fno-pic` swapped for `-fPIC`.

    `base_cflags` is the *executable* set (the kernel's static relocation model);
    a shared object is the one place the port needs PIC, so the swap is explicit
    rather than a second table.
    """
    flags = [f for f in arch.base_cflags() if f != "-fno-pic"]
    return [*flags, "-fPIC"]


def cxx_include_flags() -> list[str]:
    """The libc++ headers, ahead of the port's C headers.

    Order is what the `just libcxx-x86` note calls for: libc++ -> c-include ->
    the generated config. These use `-I`, not `-isystem`, because clang searches
    every `-I` directory before every `-isystem` one — with `-isystem` here the
    C headers would win and libc++'s `<cstring>`-style wrappers would not find
    their own `<string.h>`. `-nostdinc++` keeps the host's libstdc++ out.
    """
    return ["-nostdinc++", "-I", str(LIBCXX_INCLUDE),
            "-I", str(LIBCXXABI_INCLUDE)]


def cxx_config_flags() -> list[str]:
    """The generated `__config_site` include dir, after the C headers."""
    return ["-I", str(LIBCXX_CONFIG_INCLUDE)]


def classify(argv: list[str]) -> tuple[list[str], list[str], list[str], str | None]:
    """(compile flags, link flags, inputs, -o value) for one `cc` line."""
    comp: list[str] = []
    link: list[str] = []
    inputs: list[str] = []
    out: str | None = None
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "-o":
            i += 1
            out = argv[i] if i < len(argv) else None
        elif arg in COMPILE_VALUE_FLAGS:
            comp.append(arg)
            if i + 1 < len(argv):
                comp.append(argv[i + 1])
                i += 1
        elif arg in LINK_VALUE_FLAGS:
            link.append(arg)
            if i + 1 < len(argv):
                link.append(argv[i + 1])
                i += 1
        elif arg.startswith("-Wl,"):
            link.append(arg)
            if arg.rsplit(",", 1)[-1] in WL_OPT_TAKES_VALUE and i + 1 < len(argv):
                link.append(argv[i + 1])
                i += 1
        elif arg in LINK_FLAGS:
            link.append(arg)
        elif arg.startswith("-"):
            comp.append(arg)
        else:
            inputs.append(arg)
        i += 1
    return comp, link, inputs, out


def link_args(link: list[str]) -> list[str]:
    """`link` as LLD arguments, expanding `-Wl,` and dropping RELRO.

    meson spells a shared object's soname `-Wl,-soname,libX.so`, so a `-Wl,a,b`
    becomes `-a -b`; `-z relro` is dropped because `tools/lld.py`'s `NO_RELRO`
    (`-z norelro`) is the port's rule — a `GNU_RELRO` segment is one VM region per
    object and nothing reads it. Driver-level flags (`-pthread`, `-static`,
    `-rdynamic`) are translated or dropped, because lld is not the compiler
    driver and rejects them.
    """
    out: list[str] = []
    i = 0
    while i < len(link):
        arg = link[i]
        if arg in ("-shared", "-static", "-pthread", "-nostdlib", "-nodefaultlibs", "-s"):
            i += 1
            continue
        if arg == "-rdynamic":
            out.append("--export-dynamic")
            i += 1
            continue
        if arg == "-Wl" and i + 1 < len(link):
            parts = link[i + 1].split(",")
            i += 2
        elif arg.startswith("-Wl,"):
            parts = arg[4:].split(",")
            i += 1
        else:
            if arg == "-z" and i + 1 < len(link) and link[i + 1] == "relro":
                i += 2
                continue
            out.append(arg)
            i += 1
            continue
        j = 0
        while j < len(parts):
            if parts[j] == "-z" and j + 1 < len(parts) and parts[j + 1] == "relro":
                j += 2
                continue
            out.append(parts[j])
            j += 1
    return out


def libc_dir(arch: Arch) -> pathlib.Path:
    return ROOT / "target" / arch.triple / "release"


def ensure_libc_shims(dir: pathlib.Path) -> None:
    """Named views of `libc.so` for the libraries it already contains.

    libc++'s objects record a `-lpthread` dependency specifier (a `.deplibs`
    entry from its own build); lld follows it and fails when no `libpthread`
    exists. The same is true of `-lm`/`-ldl`/`-lrt`. A one-line linker script
    named for each forwards to `libc.so` rather than duplicating it.
    """
    for name in ("libpthread.so", "libm.so", "libdl.so", "librt.so"):
        shim = dir / name
        if not shim.exists():
            shim.write_text("INPUT ( libc.so )\n", encoding="utf-8")


def compile_object(arch: Arch, cc: str, src: str, flags: list[str],
                   out: pathlib.Path, cxx: bool = False) -> int:
    includes = cxx_include_flags() if cxx else []
    config = cxx_config_flags() if cxx else []
    return run([cc, *pic_cflags(arch), *includes, *compile_flags(), *config, *flags,
                "-c", src, "-o", str(out)])


def link(arch: Arch, lld: pathlib.Path, cc: str, comp: list[str], argv: list[str],
         out: pathlib.Path, work: pathlib.Path, cxx: bool) -> int:
    """One link, walking the original `cc` line so flag/input order survives.

    A `-Wl,--whole-archive ... -Wl,--no-whole-archive` pair only means anything
    if the archives stay between the two flags, so the link step cannot hoist all
    flags ahead of all inputs (which would leave `libdri.a` unextracted and the
    version script with nothing to export). Source inputs are compiled in place
    as they are reached.
    """
    dir = libc_dir(arch)
    if not (dir / LIBC).is_file():
        print(f"cc-dso-minix: no {dir / LIBC} — run `just dynlib-{arch.name}` first",
              file=sys.stderr)
        return 1
    ensure_libc_shims(dir)

    shared = False
    memo: dict[str, str] = {}
    args: list[str] = []
    i = 0
    n = len(argv)
    while i < n:
        arg = argv[i]
        if arg == "-o":
            i += 2
            continue
        if arg in COMPILE_VALUE_FLAGS:
            i += 2
            continue
        if arg in COMPILE_ONLY or arg in VERSION_PROBES:
            i += 1
            continue
        if arg.startswith("-Wl,") or arg == "-Wl":
            if arg == "-Wl":
                parts = argv[i + 1].split(",")
                i += 2
            else:
                parts = arg[4:].split(",")
                i += 1
            j = 0
            while j < len(parts):
                p = parts[j]
                if p == "-z" and j + 1 < len(parts) and parts[j + 1] == "relro":
                    j += 2
                    continue
                args.append(p)
                # meson passes a version script / dynamic list as the next token
                if p in WL_OPT_TAKES_VALUE and j == len(parts) - 1 and i < n:
                    args.append(argv[i])
                    i += 1
                j += 1
            continue
        if arg in LINK_VALUE_FLAGS:
            if arg == "-z" and i + 1 < n and argv[i + 1] == "relro":
                i += 2
                continue
            args.append(arg)
            if i + 1 < n:
                args.append(argv[i + 1])
                i += 2
            else:
                i += 1
            continue
        if arg in LINK_FLAGS:
            if arg == "-shared":
                shared = True
            i += 1
            continue
        if arg == "-rdynamic":
            args.append("--export-dynamic")
            i += 1
            continue
        if arg.startswith("-"):
            # meson passes linker flags through `-Wl,`, so a bare flag here is a
            # compiler flag (e.g. `-fPIC`) unless it is a path/library option.
            # `-L`/`-l` pass through; the spurious host libraries do not.
            if arg.startswith("-L") or (arg.startswith("-l") and arg not in SPURIOUS_LIBS):
                args.append(arg)
            i += 1
            continue
        # An input: an existing object/archive/shared object passes through;
        # anything else is a source to compile now.
        if arg.endswith((".o", ".a", ".so")) or ".so." in arg:
            args.append(arg)
        else:
            obj = memo.get(arg)
            if obj is None:
                obj = str(work / (pathlib.Path(arg).stem + ".o"))
                if compile_object(arch, cc, arg, comp, pathlib.Path(obj), cxx) != 0:
                    return 1
                memo[arg] = obj
            args.append(obj)
        i += 1

    cmd: list[object] = [lld, "-flavor", "gnu"]
    if shared:
        # `--no-undefined` cannot hold for a shared object the port loads: it
        # references `__tls_get_addr` (the loader) and libc symbols resolved at
        # runtime. `--allow-shlib-undefined` below is the port's rule.
        args = [a for a in args if a != "--no-undefined"]
        cmd += ["-shared", *NO_RELRO]
    else:
        cmd += [f"-T{LD_SCRIPT}", f"--dynamic-linker={INTERP}"]
        crt0 = work / "crt0.o"
        if run([cc, *pic_cflags(arch), "-c", str(arch.crt0), "-o", str(crt0)]) != 0:
            return 1
        cmd.append(str(crt0))
    cmd += ["-o", str(out), *args]
    if LIBCXX_RUNTIME.is_file():
        cmd.append(str(LIBCXX_RUNTIME))
    cmd += ["-L" + str(dir), "-l:" + LIBC, "--allow-shlib-undefined"]
    return run(cmd)


def main(argv: list[str]) -> int:
    # `--cxx` selects clang++ for the whole line: meson's cross file names this
    # wrapper once for C and once for C++, and the mode is what separates them.
    cxx = "--cxx" in argv
    argv = [a for a in argv if a != "--cxx"]
    cc = "clang++" if cxx else "clang"
    arch, argv = resolve_argv(argv)
    if not argv:
        print("cc-dso-minix: no arguments", file=sys.stderr)
        return 1
    if len(argv) == 1 and argv[0] in VERSION_PROBES:
        print(f"cc-dso-minix: {cc} for {arch.clang_target}, linked by lld against "
              f"{LIBC}")
        return 0

    lld = find_lld()
    if lld is None:
        print("cc-dso-minix: no lld to link with (see tools/lld.py)", file=sys.stderr)
        return 1

    comp, linkf, inputs, out = classify(argv)
    work = WORK_ROOT / arch.name
    work.mkdir(parents=True, exist_ok=True)

    if any(a in COMPILE_ONLY for a in argv):
        includes = cxx_include_flags() if cxx else []
        config = cxx_config_flags() if cxx else []
        cmd = [cc, *pic_cflags(arch), *includes, *compile_flags(), *config, *comp, *inputs]
        if out:
            cmd += ["-o", out]
        return run(cmd)

    if not inputs:
        # A linker probe (`cc -Wl,--version`, which meson runs to identify the
        # linker) has flags and no input file; forward it to lld, which is the
        # linker being asked about.
        if linkf:
            return run([lld, "-flavor", "gnu", *link_args(linkf)])
        print("cc-dso-minix: no input files", file=sys.stderr)
        return 1
    if out is None:
        print("cc-dso-minix: no -o for a link", file=sys.stderr)
        return 1

    return link(arch, lld, cc, comp, argv, pathlib.Path(out), work, cxx)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
