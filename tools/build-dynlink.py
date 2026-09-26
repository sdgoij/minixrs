#!/usr/bin/env python3
"""Build the Phase 0 dynamic-linking artifacts.

Produces, under `target/dynlink/`:

* `libdyn.so`, `libdyn2.so` — the shared objects (`tools/libdyn.c`,
  `tools/libdyn2.c`), built with LLD directly;
* `dynhello`   — a classic-dynamic `ET_EXEC` (`tools/dynhello.c`) with `PT_INTERP`
  (`/libexec/ld.so`) and `DT_NEEDED` for both objects.

The executable is linked with the fork's stage1 rustc as the driver (the same way
`tools/build-c-hello.py` does it), because its `write`/`exit` come from the
`minix-libc` rlib; the C objects and the shared objects are clang + LLD's work.

Phase 0 was one object; Phase 1 needs two, because the loader's *base* allocator
is only exercised when a second object has to land somewhere other than the
Phase 2 chains them: `libdyn` names `libdyn2`, so one of the objects is a
dependency of a dependency and of the executable at the same time — and names it by
a soname *and* by two paths to the same file, so that "loads once" is about the
file rather than about the spelling.

Phase 7 made this three-arch: the C objects and the executable are compiled and
linked for whichever target is named, and only the loader's own `_start` and TLS
placement differ between them (`crates/ldso`).

Usage: python tools/build-dynlink.py [x86|riscv64|aarch64]

Prerequisites: the fork's stage1 compiler and an LLD (`just bootstrap`), and
clang on PATH.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import X86_64, Arch, resolve_argv  # noqa: E402
from ccflags import compile_flags  # noqa: E402
from lld import find_lld  # noqa: E402

DSOS = ("libdyn2", "libdyn")
INTERP = "/libexec/ld.so"
OUT = ROOT / "target" / "dynlink"

# Two link-only copies of `libdyn2.o` beside the installed object, whose *sonames* are
# paths to it: `libdynpath.so` names `/lib/libdyn2.so` and `libdyndot.so` names
# `/lib/./libdyn2.so`. lld records a shared library's soname as the `DT_NEEDED` of
# whatever links it, so these are what let `libdyn.so` name one file by a path rather
# than by a name — which is the case the loader's "already loaded" check has to settle
# by the file (`st_dev`/`st_ino`) and not by the string. The dotted one is the harder
# of the two: it reaches the same inode through a path that is not even the same text
# as `/lib/libdyn2.so`.
#
# Neither copy is installed, so all three names are one file with one inode.
ALIASES = (
    ("libdynpath.so", "/lib/libdyn2.so"),
    ("libdyndot.so", "/lib/./libdyn2.so"),
)


def find_stage1_rustc() -> "pathlib.Path | None":
    build = ROOT / "rust" / "build"
    if not build.is_dir():
        return None
    found = []
    for bin_dir in sorted(build.glob("*/stage1/bin")):
        for name in ("rustc.exe", "rustc"):
            exe = bin_dir / name
            if exe.is_file():
                found.append(exe)
    if not found:
        return None
    for exe in found:
        if "windows-msvc" in str(exe):
            return exe
    return found[0]


def run(cmd: list[object], env: "dict | None" = None) -> int:
    print("running:", " ".join(str(c) for c in cmd))
    return subprocess.run([str(c) for c in cmd], env=env).returncode


def build(arch: Arch, rustc: pathlib.Path, lld: pathlib.Path) -> int:
    work = OUT / arch.name
    work.mkdir(parents=True, exist_ok=True)

    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags()]

    # crt0 and the program are non-PIC (a non-PIE executable, per the OS's
    # linker script).
    if run(["clang", *cflags, "-o", work / "crt0.o", arch.crt0]) != 0:
        return 1
    if run(["clang", *cflags, "-o", work / "dynhello.o", ROOT / "tools" / "dynhello.c"]) != 0:
        return 1

    # The shared objects must be position-independent. `libdyn2` is built first: `libdyn`
    # names it as its own DT_NEEDED, which is what makes the loader follow a dependency
    # of a dependency (and find the object the executable also names, so it is mapped
    # once — under three different names, in fact; see `ALIASES`).
    dso_flags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-fPIC", "-c", *compile_flags()]
    for name in DSOS:
        if run(["clang", *dso_flags, "-o", work / f"{name}.o", ROOT / "tools" / f"{name}.c"]) != 0:
            return 1

    # Link each .so with LLD directly (the minix target specs ask for `lld` on
    # PATH; we have the toolchain's own).
    #
    # `libdyn2.so` first: it is the file `dynhello` and `libdyn` both name, and the two
    # aliases are the same object linked again under a path soname.
    dso2 = work / "libdyn2.so"
    if run([lld, "-flavor", "gnu", "-shared", "-soname", "libdyn2.so",
            "-o", dso2, work / "libdyn2.o"]) != 0:
        return 1
    print(f"wrote {dso2}")

    alias_args = []
    for alias_name, soname in ALIASES:
        alias = work / alias_name
        if run([lld, "-flavor", "gnu", "-shared", "-soname", soname,
                "-o", alias, work / "libdyn2.o"]) != 0:
            return 1
        print(f"wrote {alias} (soname {soname})")
        alias_args += ["-L" + str(work), "-l:" + alias_name]

    # `libdyn.so` names `libdyn2` three ways: by soname — which the loader has to find
    # on its search path — and by the two paths above, which it must recognise as the
    # file it already mapped rather than mapping a second copy of. `--no-as-needed` is
    # what keeps the aliases in `DT_NEEDED`: they export exactly the same symbols as
    # `libdyn2.so`, so only the first of them would otherwise be recorded.
    dso1 = work / "libdyn.so"
    link_so = [lld, "-flavor", "gnu", "-shared", "-soname", "libdyn.so", "-o", dso1,
               work / "libdyn.o", "--no-as-needed",
               "-L" + str(work), "-l:libdyn2.so", *alias_args]
    if run(link_so) != 0:
        return 1
    print(f"wrote {dso1}")

    # minix-libc for the target: the executable's write/exit come from here.
    env = {**os.environ, "RUSTC": str(rustc)}
    if run(["cargo", "build", "-p", "minix-libc", "--target", arch.triple, "--release"], env=env) != 0:
        return 1
    deps = arch.libc_deps
    libc_rlib = max(deps.glob("libminix_libc-*.rlib"), key=lambda p: p.stat().st_mtime)

    stub = work / "link_stub.rs"
    stub.write_text(
        "#![no_std]\n"
        "#![no_main]\n"
        "\n"
        "// Force `minix-libc` into the link: the C objects call its\n"
        "// `extern \"C\"` symbols, but rustc only adds an rlib to the link\n"
        "// when the local crate references it.\n"
        "extern crate minix_libc;\n"
        "\n"
        "#[panic_handler]\n"
        "fn panic(_info: &core::panic::PanicInfo) -> ! {\n"
        "    loop {}\n"
        "}\n",
        encoding="utf-8",
    )

    out = work / "dynhello"
    # Each shared object the executable names, as a pair of `-C link-arg` entries.
    libs = [arg for name in DSOS for arg in ("-C", f"link-arg=-l:{name}.so")]
    link = [
        rustc,
        "--crate-type", "bin",
        "--target", arch.triple,
        "--edition", "2024",
        "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
        "-C", f"linker={lld}",
        "-C", f"link-arg={work / 'crt0.o'}",
        "-C", f"link-arg={work / 'dynhello.o'}",
        # Resolve the calls and the data symbol against the shared objects, and
        # record the interpreter that must run before `main`. `-Bdynamic` is
        # needed because the minix target's `crt_static_default` makes rustc pass
        # `-static`, under which lld refuses to link a `.so` at all.
        "-C", f"link-arg=-L{work}",
        "-C", "link-arg=-Bdynamic",
        *libs,
        "-C", f"link-arg=--dynamic-linker={INTERP}",
        "--extern", f"minix_libc={libc_rlib}",
        "-L", f"dependency={deps}",
        "-o", out,
        stub,
    ]
    if run(link) != 0:
        return 1
    print(f"wrote {out}")
    return 0


def main(argv: list[str]) -> int:
    arch, rest = resolve_argv(argv)
    if rest:
        sys.exit(f"error: unknown argument {rest[0]!r}")

    rustc = find_stage1_rustc()
    if rustc is None:
        print("error: the fork's stage1 compiler was not found — run `just bootstrap`",
              file=sys.stderr)
        return 1
    lld = find_lld()
    if lld is None:
        print("error: no lld to link with — run `just bootstrap`, or install LLVM",
              file=sys.stderr)
        return 1

    return build(arch, rustc, lld)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
