#!/usr/bin/env python3
"""Build the Phase 3 dynamic artifacts: a shared C library, and a C program
linked against it.

Produces, in the target's release directory (`target/<triple>/release/`, where
`BOOT_BINS` reads them, the way `tools/build-c-hello.py` puts `helloc`/`ctest`
there):

* `libc.so` — the C library the port ships statically today, built
  position-independent and linked as a `cdylib`, with `libc.so` as its soname;
* `dynclib` — a C program (`tools/dynclib.c`) linked *against* that object rather
  than the `minix-libc` rlib, so every libc symbol it calls is resolved by the
  loader at run time. The link itself is `tools/cdyn.py`'s, which is also what
  `just cdyn` uses for a program of your own, so the flags below are the ones a
  user gets and the ones this script is exercised for.

Both are in `crates/boot-image/src/manifest.rs`'s `BOOT_BINS`, so every image
carries them: `/lib/libc.so` and `/bin/dynclib`. `/libexec/ld.so` is the third,
and it comes from `cargo build -p ldso` (`just dynlib-<arch>`, which runs this
script too).

Usage: python tools/build-dynlibc.py [x86|riscv64|aarch64]

What this script builds is the library, and two things about that are not obvious:

* The shared object is built for the arch's `-elf` triple — a target of the
  fork's own (`compiler/rustc_target/src/spec/targets/*_minix_elf.rs`), which
  differs from the executable one in being `pic` and allowing dylibs. Neither is
  a flag that could be passed instead: what a `cdylib` must not contain is the
  absolute relocations of the precompiled `core`/`alloc`, and those come from the
  target's sysroot, so the target is what has to be PIC. `tools/rust-config.py`
  lists these triples with `no-std = true`, so their sysroots hold `core` and
  `alloc` and nothing else — all this object links.
* `--features so` gives the object the `panic` lang item a final artifact needs,
  and `link-arg=--soname=libc.so` is what makes a program's `DT_NEEDED` the name
  the loader searches for: `/lib/libc.so`.

Prerequisites: the fork's stage1 compiler and an LLD (`just bootstrap`), and
clang on PATH.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import Arch, resolve_argv  # noqa: E402
from lld import NO_RELRO, as_rustc_link_args, find_lld  # noqa: E402

import cdyn  # noqa: E402

OUT = ROOT / "target" / "dynlink"

# What cargo names the object, before it is given the name its soname declares.
CRATE_SO = "libminix_libc.so"
DSO = "libc.so"
# The program that links against it, as the image spells it.
PROGRAM = "dynclib"


# The stage1 lookup, the command runner and the program's link are all `tools/cdyn.py`'s,
# so this script carries no second copy of any of them.


def dyn_triple(arch: Arch) -> str:
    """The PIC triple for `arch`, whose sysroot holds `core` and `alloc`.

    The `-elf` suffix is what the fork's target list and `tools/rust-config.py`
    agree on — see the comment there for why it is not `-dyn` — and cargo's build
    directory for it follows from the name.
    """
    return f"{arch.triple}-elf"


def build(arch: Arch, rustc: pathlib.Path, lld: pathlib.Path) -> int:
    work = OUT / arch.name
    work.mkdir(parents=True, exist_ok=True)
    # The two objects an image carries go to the release directory `BOOT_BINS` reads;
    # `work` is scratch for the compile and link in between.
    release = ROOT / "target" / arch.triple / "release"
    release.mkdir(parents=True, exist_ok=True)
    triple = dyn_triple(arch)

    # The stage1 compiler is what has this target built in and a sysroot for it,
    # so it is the RUSTC here as it is in every other recipe. Cargo drives the
    # object's link so the linker can be passed explicitly — the target spec
    # names `lld` on PATH, which is not the LLD the rest of the build uses.
    env = {**os.environ, "RUSTC": str(rustc)}
    so = [
        "cargo", "rustc",
        "--release",
        "-p", "minix-libc",
        "--features", "so",
        "--target", triple,
        "--crate-type", "cdylib",
        "--",
        "-C", f"linker={lld}",
        "-C", "link-arg=--soname=libc.so",
        *as_rustc_link_args(NO_RELRO),
    ]
    if cdyn.run(so, env=env) != 0:
        return 1

    built = ROOT / "target" / triple / "release" / CRATE_SO
    if not built.is_file():
        print(f"error: {built} was not produced", file=sys.stderr)
        return 1
    dest = release / DSO
    shutil.copyfile(built, dest)
    print(f"wrote {dest}")

    # And the program that links against it. The link is `tools/cdyn.py`'s, so the shipped
    # `/bin/dynclib` and a program built by `just cdyn` are built by the same flags —
    # including the ones that are easy to leave out (`-Bdynamic` against the target's
    # static default, and `--allow-shlib-undefined` for the four symbols the loader
    # answers for an object).
    return cdyn.link_program(
        arch, ROOT / "tools" / "dynclib.c", release / PROGRAM, rustc, lld, release, work
    )


def main(argv: list[str]) -> int:
    arch, rest = resolve_argv(argv)
    if rest:
        sys.exit(f"error: unknown argument {rest[0]!r}")

    rustc = cdyn.find_stage1_rustc()
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
