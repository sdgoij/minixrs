#!/usr/bin/env python3
"""Link a C program against the port's shared C library.

An image carries `/lib/libc.so` and the loader (`crates/boot-image/src/manifest.rs`), so a
program linked this way runs on a booted system with nothing else added to the image. The
program is a normal, non-PIC minix executable — VFS execs it at its link addresses like
any other — and what makes it dynamic is three things in the link:

* `-Bdynamic`, because the minix target's `crt_static_default` makes rustc pass `-static`,
  under which lld refuses a shared object at all;
* `-l:libc.so` against the directory holding it, which is what gives the program a
  `DT_NEEDED` the loader can find (`/lib/libc.so` on the search path);
* `--dynamic-linker=/libexec/ld.so`, which is the `PT_INTERP` the kernel's exec looks for.

`--allow-shlib-undefined` comes with the second: `libc.so` leaves four symbols undefined
for the loader (`__tls_get_addr` and the three TLS bounds) and lld checks a linked shared
object's undefined symbols by default.

This module is shared: `tools/build-dynlibc.py` builds the shipped `/bin/dynclib` with
[`link_program`], and `just cdyn` is it for a program of your own.

Usage: python tools/cdyn.py [arch] <source.c> [name]
Writes `target/dync/<arch>/<name>`, `name` defaulting to the source's stem, and prints the
two commands that boot it.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import Arch, resolve_argv  # noqa: E402
from ccflags import compile_flags  # noqa: E402
from lld import find_lld  # noqa: E402

INTERP = "/libexec/ld.so"
LIBC = "libc.so"

# `rustc` drives the link (the C objects alone are not a crate), and the crate it is given
# exists only for that: it must not name `minix_libc`, or the link would put a second,
# static copy of the C library in the program.
STUB = (
    "#![no_std]\n"
    "#![no_main]\n"
    "\n"
    "// Written by tools/cdyn.py: rustc needs a crate to link the C objects into. It\n"
    "// deliberately does not name `minix_libc` — the C library is meant to arrive from\n"
    "// `libc.so` at run time, and an rlib here would be a static copy of it.\n"
    "#[panic_handler]\n"
    "fn panic(_info: &core::panic::PanicInfo) -> ! {\n"
    "    loop {}\n"
    "}\n"
)


def run(cmd: list[object], env: "dict | None" = None) -> int:
    print("running:", " ".join(str(c) for c in cmd))
    return subprocess.run([str(c) for c in cmd], env=env).returncode


def find_stage1_rustc() -> "pathlib.Path | None":
    """The fork's stage1 `rustc`, or None.

    The same lookup `tools/build-dynlibc.py` and `tools/build-dynlink.py` do: the stage1
    compiler is the one with the minix targets built in and a sysroot for them.
    """
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


def link_program(
    arch: Arch,
    source: pathlib.Path,
    out: pathlib.Path,
    rustc: pathlib.Path,
    lld: pathlib.Path,
    libc_dir: pathlib.Path,
    scratch: pathlib.Path,
) -> int:
    """Compile `source` and link it against the `libc.so` in `libc_dir`.

    `scratch` holds the objects in between, which is all they are: the program is `out`.
    """
    scratch.mkdir(parents=True, exist_ok=True)
    out.parent.mkdir(parents=True, exist_ok=True)

    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags()]
    crt0 = scratch / "crt0.o"
    obj = scratch / f"{source.stem}.o"
    for src, dst in ((arch.crt0, crt0), (source, obj)):
        if run(["clang", *cflags, "-o", dst, src]) != 0:
            return 1

    stub = scratch / f"{source.stem}_stub.rs"
    stub.write_text(STUB, encoding="utf-8")

    link = [
        rustc,
        "--crate-type", "bin",
        "--target", arch.triple,
        "--edition", "2024",
        "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
        "-C", f"linker={lld}",
        "-C", f"link-arg={crt0}",
        "-C", f"link-arg={obj}",
        "-C", f"link-arg=-L{libc_dir}",
        "-C", "link-arg=-Bdynamic",
        "-C", "link-arg=--allow-shlib-undefined",
        "-C", f"link-arg=-l:{LIBC}",
        "-C", f"link-arg=--dynamic-linker={INTERP}",
        "-o", out,
        stub,
    ]
    if run(link) != 0:
        return 1
    print(f"wrote {out}")
    return 0


def main(argv: list[str]) -> int:
    arch, rest = resolve_argv(argv)
    if not rest:
        sys.exit("error: needs a C source (usage: python tools/cdyn.py [arch] <source.c> [name])")
    if len(rest) > 2:
        sys.exit(f"error: unexpected argument {rest[2]!r}")
    source = pathlib.Path(rest[0])
    if not source.is_file():
        sys.exit(f"error: no such source: {source}")
    name = rest[1] if len(rest) > 1 else source.stem

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

    work = ROOT / "target" / "dync" / arch.name
    out = work / name
    # The library is the one the image ships: `tools/build-dynlibc.py` puts it in the
    # release directory, which is where `BOOT_BINS` reads it from.
    libc_dir = ROOT / "target" / arch.triple / "release"
    if not (libc_dir / LIBC).is_file():
        print(f"error: no {libc_dir / LIBC} — run `just dynlib-{arch.name}` first",
              file=sys.stderr)
        return 1

    if link_program(arch, source, out, rustc, lld, libc_dir, work) != 0:
        return 1
    # Relative to the workspace, which is how the recipes name things and how
    # `MINIXFS_EXTRA` resolves a value (`crates/kernel/build.rs`). On Windows the value
    # must also be excluded from MSYS's path conversion, or `/bin/<name>` arrives as the
    # MSYS `/bin` — the trap `crates/kernel/build.rs` refuses with a message naming it,
    # and the Justfile's own recipes avoid by setting the variable inside a recipe.
    excl = "MSYS2_ENV_CONV_EXCL=MINIXFS_EXTRA " if os.name == "nt" else ""
    print(f"run it with: {excl}MINIXFS_EXTRA='/bin/{name}={out.relative_to(ROOT).as_posix()}'"
          f" just run {arch.name}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
