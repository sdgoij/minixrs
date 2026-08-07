#!/usr/bin/env python3
"""Rebuild the std smoke-test binary (`/bin/hello`) from the rust fork's stage1 compiler.

Usage: python tools/build-std-hello.py [all|x86|riscv64|aarch64|<triple>]
                                       (default: x86_64-pc-minix)

Arch names and triples are both accepted:
`target` is one of the fork's in-tree minix triples (default
`x86_64-pc-minix`):

  - x86_64-pc-minix          -> target/x86_64-pc-minix/release/hello
  - riscv64gc-unknown-minix  -> target/riscv64gc-unknown-minix/release/hello
  - aarch64-unknown-minix    -> target/aarch64-unknown-minix/release/hello

The OS release output directory is the triple itself for all three (see
`crates/boot-image/src/targets.rs`); the std smoke test uses the fork's
in-tree spec, which carries a built std.

Prerequisites:
  1. The fork's std for the chosen target must be built first:
       just bootstrap [x86|riscv64|aarch64]
     (or manually: cd rust && python x.py build library/std --target <triple>)
     This produces the stage1 compiler + the minix std sysroot under
     `rust/build/<host-triple>/stage1/`.
  2. After (re)building the binary, re-assemble the boot images so
     `/bin/hello` is embedded, then refresh the disk image:
       target/mkboot embed_initramfs,embed_minixfs
       target/mkfs <arch>
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SOURCE = ROOT / "tools" / "std-hello.rs"

# Short arch names (`just bootstrap` vocabulary) -> rustc triple.
ARCHES = {
    "x86": "x86_64-pc-minix",
    "riscv64": "riscv64gc-unknown-minix",
    "aarch64": "aarch64-unknown-minix",
}

# rustc triple -> OS release output directory (the dir
# `crates/kernel/build.rs` reads userland binaries from).
TARGETS = {
    "x86_64-pc-minix": "x86_64-pc-minix",
    "riscv64gc-unknown-minix": "riscv64gc-unknown-minix",
    "aarch64-unknown-minix": "aarch64-unknown-minix",
}


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


def build(target: str) -> int:
    out_dir = TARGETS.get(target)
    if out_dir is None:
        print(
            f"error: unknown target {target!r}; expected one of "
            + ", ".join(sorted(ARCHES) + sorted(TARGETS))
            + ", or all",
            file=sys.stderr,
        )
        return 2

    rustc = find_stage1_rustc()
    if rustc is None:
        print(
            "error: rust fork stage1 compiler not found — build it first "
            "(see module docstring)",
            file=sys.stderr,
        )
        return 1

    out = ROOT / "target" / out_dir / "release" / "hello"
    out.parent.mkdir(parents=True, exist_ok=True)
    cmd = [str(rustc), "--target", target, "--edition", "2024", "-o", str(out), str(SOURCE)]
    print("running:", " ".join(cmd))
    result = subprocess.run(cmd)
    if result.returncode != 0:
        print(f"error: rustc failed with exit code {result.returncode}", file=sys.stderr)
        return result.returncode
    print(f"wrote {out}")
    return 0


def main(argv: list[str]) -> int:
    arg = argv[1] if len(argv) > 1 else "x86_64-pc-minix"
    if arg == "all":
        for triple in ARCHES.values():
            rc = build(triple)
            if rc != 0:
                return rc
        return 0
    if arg in ARCHES:
        arg = ARCHES[arg]
    return build(arg)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
