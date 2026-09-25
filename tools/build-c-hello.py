#!/usr/bin/env python3
"""Build the C smoke-test binaries (`/bin/helloc`, `/bin/ctest`) with the
minix fork toolchain.

Compiles the freestanding C sources (`tools/hello.c`, `tools/ctest.c`,
`tools/crt0-<arch>.S`) with clang, builds the `minix-libc` rlib (and its
no_std `minix-std`/`net`/`minix-rt` deps) with the fork's stage1 compiler for
the target's minix triple, then links each program with the fork rustc as the
driver (it resolves the rlib metadata and the minix sysroot's
core/compiler_builtins).

All of the C library surface (printf family, strtod, wcs*, ...) lives in
`minix-libc`; the C objects only supply the program code and `_start`.

Usage: python tools/build-c-hello.py [all|x86|riscv64|aarch64|<triple>]
                                    (default: x86_64-pc-minix)

Outputs: `target/<triple>/release/{helloc,ctest}` — the locations the
boot-image assembly reads `/bin/helloc`/`/bin/ctest` from
(`crates/boot-image/src/manifest.rs`). Those entries are x86_64-only today, so
on the other arches these binaries are built for their own sake (a C surface
check) and injected with `MINIXFS_EXTRA` where a recipe wants to run one.

Prerequisites:
  1. The fork's stage1 compiler + minix sysroot (`just bootstrap`).
  2. After building, re-assemble the boot images so the binaries are
     embedded:
       target/mkboot embed_initramfs,embed_minixfs
       target/mkfs x86_64
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import ALL, Arch, resolve_argv  # noqa: E402
from lld import find_lld  # noqa: E402
from ccflags import compile_flags  # noqa: E402

# (source, output name)
PROGRAMS = [
    (ROOT / "tools" / "hello.c", "helloc"),
    (ROOT / "tools" / "ctest.c", "ctest"),
]

# Scratch objects, per arch: two arches' objects in one directory would be
# linked into each other's executable.
WORK_ROOT = ROOT / "target" / "c-hello"


def find_stage1_rustc() -> "pathlib.Path | None":
    """Locate the fork's stage1 rustc under `rust/build/<host-triple>/stage1/`.

    The host triple differs per build machine (x86_64-pc-windows-msvc here,
    x86_64-unknown-linux-gnu on Linux, ...), so prefer the common
    windows-msvc triple and fall back to any other stage1 found.
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


def run(cmd: list[object], env: "dict | None" = None) -> int:
    print("running:", " ".join(str(c) for c in cmd))
    return subprocess.run([str(c) for c in cmd], env=env).returncode


def build(arch: Arch, rustc: pathlib.Path, lld: pathlib.Path) -> int:
    out_dir = ROOT / "target" / arch.triple / "release"
    work = WORK_ROOT / arch.name
    out_dir.mkdir(parents=True, exist_ok=True)
    work.mkdir(parents=True, exist_ok=True)

    cflags = [
        *arch.base_cflags(),
        "-fno-builtin",
        "-O2",
        "-c",
        *compile_flags(),
    ]
    if run(["clang", *cflags, "-o", work / "crt0.o", arch.crt0]) != 0:
        return 1
    for src, _ in PROGRAMS:
        if run(["clang", *cflags, "-o", work / f"{src.stem}.o", src]) != 0:
            return 1

    # 2. Build minix-libc (+ minix-std/net/minix-rt) for the minix target.
    env = {**os.environ, "RUSTC": str(rustc)}
    if run(["cargo", "build", "-p", "minix-libc", "--target", arch.triple,
            "--release"], env=env) != 0:
        return 1

    # 3. Link each program with the fork rustc as the driver. A
    #    `#![no_std] #![no_main]` stub provides the crate; `_start` (and
    #    `main`) come from the C objects, the C library surface from the
    #    minix-libc rlib.
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
    for src, name in PROGRAMS:
        out = out_dir / name
        link = [
            rustc,
            "--crate-type", "bin",
            "--target", arch.triple,
            "--edition", "2024",
            "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
            # The minix target specs ask rustc for a program named `lld` on
            # PATH; the toolchain's own LLD is not called that (see tools/lld.py).
            "-C", f"linker={lld}",
            "-C", f"link-arg={work / 'crt0.o'}",
            "-C", f"link-arg={work / f'{src.stem}.o'}",
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
    if argv and argv[0] == "all":
        arches = list(ALL)
    else:
        arch, rest = resolve_argv(argv)
        if rest:
            sys.exit(f"error: unknown argument {rest[0]!r} (see the docstring)")
        arches = [arch]

    rustc = find_stage1_rustc()
    if rustc is None:
        print(
            "error: rust fork stage1 compiler not found — build it first "
            "(see module docstring)",
            file=sys.stderr,
        )
        return 1

    lld = find_lld()
    if lld is None:
        print(
            "error: no lld to link with — build the toolchain (`just bootstrap`) "
            "so its LLD lands in the sysroot, or install LLVM",
            file=sys.stderr,
        )
        return 1

    for arch in arches:
        rc = build(arch, rustc, lld)
        if rc != 0:
            return rc
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
