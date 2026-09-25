#!/usr/bin/env python3
"""Locate an `lld` for linking the minix targets.

The fork's minix target specs are GNU-flavoured LLD targets, so rustc links a
minix binary by running a program named `lld`, resolved on `PATH`. Nothing
guarantees that name exists: a CI image has no system LLVM, and the LLD a
toolchain does ship sits in its sysroot as `rust-lld`, a name rustc only looks
for when the target spec asks for it. Everything that links minix code
therefore asks this module where the linker is and passes the path on:
`CARGO_TARGET_<TRIPLE>_LINKER` from the Justfile, `-C linker=` in the scripts
that invoke rustc directly.

Candidates, in order:

1. `MINIXRS_LLD` - an explicit override, mainly for the self-test.
2. The stage1 sysroot's `rust-lld` (`lib/rustlib/<host>/bin/rust-lld`): the LLD
   built by the same x.py run as the compiler it links for. Linux hosts always
   have it, because bootstrap defaults its host to the self-contained LLD
   linker and so enables `rust.lld`.
3. `rust/build/<host>/lld/bin/lld` - LLD of a from-source LLVM build.
4. `rust/build/<host>/ci-llvm/bin/lld` - LLD out of the downloaded CI LLVM.
5. `lld` on `PATH` - a system LLVM install.

Usage: python tools/lld.py
Prints the linker path, or nothing when there is none: whether that is fatal
depends on the caller. `just bootstrap` on a host that has not built LLD yet
still has to be able to parse the Justfile, and only links afterwards.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "rust" / "build"


def exe(name: str) -> str:
    """`name` as an executable file name on this platform."""
    return name + ".exe" if os.name == "nt" else name


def host_triple() -> str:
    """The build host triple, which is the name of x.py's build directory.

    Empty when `rustc` cannot be run: the lookup then trusts `build/host`.
    """
    try:
        out = subprocess.run(
            ["rustc", "-vV"], capture_output=True, text=True, check=True
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return ""
    for line in out.splitlines():
        if line.startswith("host: "):
            return line[6:].strip()
    return ""


def build_dirs(host: str) -> "list[pathlib.Path]":
    """x.py's build directory for `host`, then x.py's `build/host` symlink.

    A build directory for another host holds a compiler - and an LLD - that
    this machine cannot run, so it is never considered. The symlink covers a
    host triple that could not be detected.
    """
    dirs = []
    for name in (host, "host"):
        path = BUILD / name
        if name and path.is_dir() and path not in dirs:
            dirs.append(path)
    return dirs


def find_nm() -> "pathlib.Path | None":
    """The `llvm-nm` to read a minix image's symbols with, or None.

    Same candidates as `find_lld`, and for the same reason: the LLVM a build
    downloaded is where the linker came from, and its `llvm-nm` is beside it. The
    host tools use it to read a struct layout out of the image rather than
    hardcoding one (`tools/dso_share_probe.py`).
    """
    override = os.environ.get("MINIXRS_LLVM_NM")
    if override:
        path = pathlib.Path(override)
        if not path.is_file():
            # Not a reason to silently read the symbols with something else.
            print(
                f"error: MINIXRS_LLVM_NM is {override}, which is not a file",
                file=sys.stderr,
            )
            return None
        return path

    for build in build_dirs(host_triple()):
        for bin_dir in (build / "ci-llvm" / "bin", build / "lld" / "bin"):
            nm = bin_dir / exe("llvm-nm")
            if nm.is_file():
                return nm

    system = shutil.which("llvm-nm")
    return pathlib.Path(system) if system else None


def find_lld() -> "pathlib.Path | None":
    """The lld to link minix binaries with, or None when there is none."""
    override = os.environ.get("MINIXRS_LLD")
    if override:
        path = pathlib.Path(override)
        if not path.is_file():
            # Not a reason to silently link with something else.
            print(
                f"error: MINIXRS_LLD is {override}, which is not a file",
                file=sys.stderr,
            )
            return None
        return path

    host = host_triple()
    for build in build_dirs(host):
        # The toolchain's own LLD, installed in the sysroot of the host it was
        # built on.
        sysroot_bin = build / "stage1" / "lib" / "rustlib" / (host or build.name) / "bin"
        rust_lld = sysroot_bin / exe("rust-lld")
        if rust_lld.is_file():
            return rust_lld

        # Otherwise the LLD an LLVM build left behind, under the bare `lld`
        # name rustc asks for: from-source first, then the CI artifacts.
        for bin_dir in (build / "lld" / "bin", build / "ci-llvm" / "bin"):
            lld = bin_dir / exe("lld")
            if lld.is_file():
                return lld

    system = shutil.which("lld")
    return pathlib.Path(system) if system else None


def main() -> int:
    path = find_lld()
    if path is not None:
        print(path)
    return 0


if __name__ == "__main__":
    sys.exit(main())
