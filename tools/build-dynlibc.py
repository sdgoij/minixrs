#!/usr/bin/env python3
"""Build the Phase 3 dynamic artifacts: a shared C library, and a C program
linked against it.

Produces, under `target/dynlink/<arch>/`:

* `libc.so` — the C library the port ships statically today, built
  position-independent and linked as a `cdylib`, with `libc.so` as its soname;
* `dynclib` — a C program (`tools/dynclib.c`) linked *against* that object rather
  than the `minix-libc` rlib, so every libc symbol it calls is resolved by the
  loader at run time.

Both are x86_64-only, like the loader itself (D1 of `DYNAMIC_LINKING.md`).
Usage: python tools/build-dynlibc.py [x86]

Three things about the build are not obvious:

* The shared object needs `core` and `alloc` built *position-independent*. The
  minix target's sysroot `libcore` is not, and the link fails with
  `R_X86_64_64 cannot be used against local symbol` until `-Z build-std` rebuilds
  them under a PIC target — which is why `tools/minix-dyn-target/` exists rather
  than a `-C` flag: the target's `crt_static_allows_dylibs` is also what stops
  rustc from dropping the `cdylib` crate type outright
  (`rustc_session/src/output.rs::invalid_output_for_target`).
* `--features so` gives the object the `panic` lang item a final artifact needs.
* `link-arg=--soname=libc.so` is what makes the program's `DT_NEEDED` the name the
  loader searches for: `/lib/libc.so`.

Prerequisites: a nightly cargo with `rust-src` (for `build-std`), the fork's
stage1 compiler and an LLD (`just bootstrap`), and clang on PATH.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import X86_64, Arch, resolve_argv  # noqa: E402
from ccflags import compile_flags  # noqa: E402
from lld import find_lld  # noqa: E402

INTERP = "/libexec/ld.so"
OUT = ROOT / "target" / "dynlink"
DYN_TARGET = ROOT / "tools" / "minix-dyn-target" / "x86_64-pc-minix-dyn.json"
# The `cdylib` build gets a cargo target directory of its own: `-Z build-std`
# builds a whole sysroot, and keeping it here keeps those PIC objects out of
# `target/<triple>`, which the static userland is built from (D2).
BUILD_DIR = ROOT / "target" / "dynlibc"

# What cargo names the object, before it is given the name its soname declares.
CRATE_SO = "libminix_libc.so"
DSO = "libc.so"


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

    # Cargo drives the object's link so it can find the sysroot it just built
    # (`target/dynlibc/.../build/core/<hash>/out`) instead of the path being
    # spelled out here — it is a cargo-internal detail. The linker is passed
    # explicitly for the same reason as everywhere else: the target spec asks for
    # `lld` on PATH, which is not the LLD the rest of the build uses.
    env = {**os.environ, "CARGO_TARGET_DIR": str(BUILD_DIR)}
    so = [
        "cargo", "+nightly", "rustc",
        "-Z", "json-target-spec",
        "-Z", "build-std=core,alloc",
        "--release",
        "-p", "minix-libc",
        "--features", "so",
        "--target", str(DYN_TARGET),
        "--crate-type", "cdylib",
        "--",
        "-C", f"linker={lld}",
        "-C", "link-arg=--soname=libc.so",
    ]
    if run(so, env=env) != 0:
        return 1

    built = BUILD_DIR / "x86_64-pc-minix-dyn" / "release" / CRATE_SO
    if not built.is_file():
        print(f"error: {built} was not produced", file=sys.stderr)
        return 1
    dest = work / DSO
    shutil.copyfile(built, dest)
    print(f"wrote {dest}")

    # The program is non-PIC, like every other minix executable: the OS's linker
    # script places it and the kernel maps it at its link addresses.
    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags()]
    if run(["clang", *cflags, "-o", work / "crt0.o", arch.crt0]) != 0:
        return 1
    if run(["clang", *cflags, "-o", work / "dynclib.o", ROOT / "tools" / "dynclib.c"]) != 0:
        return 1

    stub = work / "dynclib_stub.rs"
    stub.write_text(
        "#![no_std]\n"
        "#![no_main]\n"
        "\n"
        "// The program itself is C; this crate exists only to give rustc something\n"
        "// to drive the link with. It deliberately does not name `minix_libc`: the C\n"
        "// library is meant to arrive from `libc.so` at run time, and an rlib here\n"
        "// would put a second, static copy of it in the image.\n"
        "#[panic_handler]\n"
        "fn panic(_info: &core::panic::PanicInfo) -> ! {\n"
        "    loop {}\n"
        "}\n",
        encoding="utf-8",
    )

    out = work / "dynclib"
    link = [
        rustc,
        "--crate-type", "bin",
        "--target", arch.triple,
        "--edition", "2024",
        "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
        "-C", f"linker={lld}",
        "-C", f"link-arg={work / 'crt0.o'}",
        "-C", f"link-arg={work / 'dynclib.o'}",
        # `-l:libc.so` is the name the object's `DT_NEEDED` records, and
        # `-Bdynamic` is not optional: the minix target's `crt_static_default`
        # makes rustc pass `-static`, under which lld refuses a shared object.
        #
        # The object leaves four symbols undefined on purpose — `__tls_get_addr`
        # and the three TLS bounds the loader answers — and lld checks a linked
        # shared object's undefined symbols by default, so it has to be told they
        # are the loader's to resolve.
        "-C", f"link-arg=-L{work}",
        "-C", "link-arg=-Bdynamic",
        "-C", "link-arg=--allow-shlib-undefined",
        "-C", "link-arg=-l:libc.so",
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
    if rest:
        sys.exit(f"error: unknown argument {rest[0]!r}")
    if arch is not X86_64:
        sys.exit("error: Phase 3 is x86_64 only (the loader's _start is x86_64)")

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
