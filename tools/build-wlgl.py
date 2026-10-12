#!/usr/bin/env python3
"""Build `/bin/wlgl` — the Wayland GL present client (.notes/wayland.md §6.10, 3d-a).

This is the first *dynamically linked Rust* program in the tree, and it has to be
one. `dlopen` needs a loader in the address space, and a statically linked minix
image has none: `crates/minix-libc/src/lib.rs` answers NULL for `dlopen` there by
design. Mesa is only reachable through the loader, so any program that renders with
GL — a client here, and a compositor later — must be a dynamic image.

The link is the same shape `tools/build-gltest.py` and `tools/cdyn.py` give their C
clients: the arch's C `crt0.o` provides `_start`, `minix-user.ld` lays the image out,
and `-Bdynamic` + `--dynamic-linker=/libexec/ld.so` make `libc.so`, `libEGL.so.1` and
`libGLESv2.so.2` resolve at run time. What differs is the Rust half: the Wayland
protocol client is `crates/userland`'s `wlclient` module, linked in as an rlib, so the
protocol stays in `crates/wayland` and is not re-implemented in C.

Usage: python tools/build-wlgl.py [x86]
Writes `target/mesa/<arch>/bin/wlgl`.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import resolve_argv  # noqa: E402
from ccflags import compile_flags  # noqa: E402
from lld import find_lld  # noqa: E402
import cdyn  # noqa: E402

INTERP = "/libexec/ld.so"
LIBC = "libc.so"
RENDER = ROOT / "tools" / "wlgl_render.c"

# The crate rustc links the `userland` rlib and the GL object into. Its `main` is
# what the crt0 below calls; the crt0 ends the image on `exit`.
#
# No `#[panic_handler]` here: `minix-rt` brings one in through `minix-std`, and a
# second definition is a duplicate lang item (E0152).
STUB = '''#![no_std]
#![no_main]

// Written by tools/build-wlgl.py.

#[unsafe(no_mangle)]
pub extern "C" fn main(argc: i32, argv: *const *const u8, _envp: *const *const u8) -> i32 {
    let mut buf = [""; 64];
    let args = unsafe { userland::parse_args(argc, argv, &mut buf) };
    userland::wlclient::wl_gl(args)
}
'''

# The C crt0's entry symbol, renamed at compile time. `minix-rt` defines `_start`
# too (its own runtime's entry, unused here but pulled in with `minix-std`), and two
# definitions of it do not link. Renaming this one and pointing the ELF entry at it
# with `-e` leaves the crt0 as the entry — it is what sets up the thread pointer and
# `environ` through `libc.so`, which a dynamic image needs.
CRT0_ENTRY = "minix_crt0_entry"


def run(cmd: list[object], env: "dict | None" = None) -> int:
    print("running:", " ".join(str(c) for c in cmd))
    return subprocess.run([str(c) for c in cmd], env=env).returncode


def build_rlib(arch, rustc) -> int:
    """Build `libuserland.rlib`, the crate the link below pulls in.

    `just userland-<arch>` builds `--bins`, which leaves only the hashed copy under
    `deps/`; the lib target is what puts the canonical name where `--extern` wants it.
    """
    cargo = shutil.which("cargo")
    if cargo is None:
        print("error: no cargo on PATH", file=sys.stderr)
        return 1
    env = {**os.environ, "RUSTC": str(rustc)}
    return run([cargo, "build", "-p", "userland", "--lib", "--release",
                "--target", arch.triple], env=env)


def main(argv: list[str]) -> int:
    arch, rest = resolve_argv(argv)
    if rest:
        sys.exit(f"error: unexpected argument {rest[0]!r}")

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

    release = ROOT / "target" / arch.triple / "release"
    userland = release / "libuserland.rlib"
    if build_rlib(arch, rustc) != 0:
        return 1
    mesa_lib = ROOT / "target" / "mesa" / arch.name / "lib"
    # The headers come from the Mesa source checkout (`just build-mesa`) or from the
    # published artifact (`just fetch-mesa`).
    mesa_include = ROOT / "target" / "mesa" / arch.name / "include"
    if not (mesa_include / "EGL" / "egl.h").is_file():
        mesa_include = ROOT / "target" / "mesa-src" / "include"

    for path, why in (
        (userland, f"run `just build-x86` first, which builds it"),
        (release / LIBC, f"run `just dynlib-{arch.name}` first"),
        (mesa_lib / "libEGL.so.1",
         f"run `just build-mesa {arch.name} --stage`, or `just fetch-mesa {arch.name}`"),
        (mesa_include / "EGL" / "egl.h",
         f"run `just build-mesa {arch.name} --build`, or `just fetch-mesa {arch.name}`"),
    ):
        if not path.is_file():
            print(f"error: no {path} — {why}", file=sys.stderr)
            return 1

    work = ROOT / "target" / "mesa" / arch.name / "bin"
    work.mkdir(parents=True, exist_ok=True)
    out = work / "wlgl"
    scratch = work / "obj"
    scratch.mkdir(parents=True, exist_ok=True)

    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags(),
              f"-I{mesa_include}", "-DEGL_NO_PLATFORM_SPECIFIC_TYPES"]
    crt0 = scratch / "crt0.o"
    obj = scratch / "wlgl_render.o"
    # The crt0's `_start` is renamed; `.S` goes through the C preprocessor, so `-D`
    # is enough (see `CRT0_ENTRY`).
    if run(["clang", *cflags, f"-D_start={CRT0_ENTRY}", "-o", crt0, arch.crt0]) != 0:
        return 1
    if run(["clang", *cflags, "-o", obj, RENDER]) != 0:
        return 1

    stub = scratch / "wlgl_stub.rs"
    stub.write_text(STUB, encoding="utf-8")

    link = [
        rustc,
        "--crate-type", "bin",
        "--target", arch.triple,
        "--edition", "2024",
        "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
        "-C", f"linker={lld}",
        # The entry is the crt0's renamed `_start`, not `minix-rt`'s: `minix-user.ld`
        # names `_start`, and `-e` overrides it.
        "-C", "link-arg=-e",
        "-C", f"link-arg={CRT0_ENTRY}",
        "-C", f"link-arg={crt0}",
        "-C", f"link-arg={obj}",
        "-C", f"link-arg=-L{release}",
        "-C", f"link-arg=-L{mesa_lib}",
        # rustc's own crate search, for `userland`'s rlib and its dependencies —
        # `-C link-arg=-L` reaches only the linker.
        "-L", f"dependency={arch.libc_deps}",
        "-C", "link-arg=-Bdynamic",
        "-C", "link-arg=--allow-shlib-undefined",
        "-C", f"link-arg=-l:{LIBC}",
        "-C", "link-arg=-l:libEGL.so.1",
        "-C", "link-arg=-l:libGLESv2.so.2",
        "-C", f"link-arg=--dynamic-linker={INTERP}",
        "--extern", f"userland={userland}",
        "-o", out,
        stub,
    ]
    if run(link) != 0:
        return 1
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
