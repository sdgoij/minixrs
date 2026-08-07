#!/usr/bin/env python3
"""Build the C smoke-test binary (`/bin/helloc`) with the minix fork toolchain.

Compiles `tools/hello.c` + `tools/crt0-x86_64.S` with clang for a
freestanding x86_64 target, builds the `minix-libc` rlib (and its no_std
`minix-std`/`net`/`minix-rt` deps) with the fork's stage1 compiler for
`x86_64-pc-minix`, then links everything with the fork rustc as the driver
(it resolves the rlib metadata and the minix sysroot's core/compiler_builtins).

Output: `target/x86_64-pc-minix/release/helloc` — the location the boot-image
assembly reads `/bin/helloc` from (`crates/boot-image/src/manifest.rs`).

Prerequisites:
  1. The fork's stage1 compiler + minix sysroot (`just bootstrap`).
  2. After building, re-assemble the boot images so `/bin/helloc` is embedded:
       target/mkboot embed_initramfs,embed_minixfs
       target/mkfs x86_64
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
TARGET = "x86_64-pc-minix"
HELLO_C = ROOT / "tools" / "hello.c"
CRT0_S = ROOT / "tools" / "crt0-x86_64.S"
OUT = ROOT / "target" / TARGET / "release" / "helloc"
WORK = ROOT / "target" / "c-hello"


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


def main() -> int:
    rustc = find_stage1_rustc()
    if rustc is None:
        print(
            "error: rust fork stage1 compiler not found — build it first "
            "(see module docstring)",
            file=sys.stderr,
        )
        return 1

    OUT.parent.mkdir(parents=True, exist_ok=True)
    WORK.mkdir(parents=True, exist_ok=True)

    # 1. Freestanding compile of the C program and crt0. `-mno-red-zone`
    #    matches the fork spec's `disable_redzone`; `-fno-pic` matches the
    #    kernel's static relocation model.
    cflags = [
        "--target=x86_64-unknown-none",
        "-ffreestanding",
        "-mno-red-zone",
        "-fno-stack-protector",
        "-fno-builtin",
        "-fno-pic",
        "-O2",
        "-c",
    ]
    for src, obj in ((CRT0_S, WORK / "crt0.o"), (HELLO_C, WORK / "hello.o")):
        if run(["clang", *cflags, "-o", obj, src]) != 0:
            return 1

    # 2. Build minix-libc (+ minix-std/net/minix-rt) for the minix target.
    env = {**os.environ, "RUSTC": str(rustc)}
    if run(["cargo", "build", "-p", "minix-libc", "--target", TARGET, "--release"], env=env) != 0:
        return 1

    # 3. Link with the fork rustc as the driver. A `#![no_std] #![no_main]`
    #    stub provides the crate; `_start` (and `main`) come from the C
    #    objects, `exit`/`write` from the minix-libc rlib.
    stub = WORK / "link_stub.rs"
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
    deps = ROOT / "target" / TARGET / "release" / "deps"
    libc_rlib = max(deps.glob("libminix_libc-*.rlib"), key=lambda p: p.stat().st_mtime)
    link = [
        rustc,
        "--crate-type", "bin",
        "--target", TARGET,
        "--edition", "2024",
        "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
        "-C", f"link-arg={WORK / 'crt0.o'}",
        "-C", f"link-arg={WORK / 'hello.o'}",
        "--extern", f"minix_libc={libc_rlib}",
        "-L", f"dependency={deps}",
        "-o", OUT,
        stub,
    ]
    if run(link) != 0:
        return 1
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
