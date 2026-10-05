#!/usr/bin/env python3
"""Build the surfaceless-EGL GLES2 triangle (§6.10 3c-2) against Mesa's DSOs.

The client is a normal dynamic minix executable, the same shape `tools/cdyn.py`
builds (non-PIC, `PT_INTERP` = the loader, `DT_NEEDED` libs resolved at run
time), with two additions: Mesa's headers on the compile line, and
`-l:libEGL.so.1 -l:libGLESv2.so.2` on the link, read from the staged
`target/mesa/<arch>/lib` (`just build-mesa <arch> --stage`).

Usage: python tools/build-gltest.py [x86]
Writes `target/mesa/<arch>/bin/gltriangle`.
"""

from __future__ import annotations

import pathlib
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
SOURCE = ROOT / "tools" / "gl_triangle.c"


def run(cmd: list[object]) -> int:
    print("running:", " ".join(str(c) for c in cmd))
    return subprocess.run([str(c) for c in cmd]).returncode


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

    libc_dir = ROOT / "target" / arch.triple / "release"
    mesa_lib = ROOT / "target" / "mesa" / arch.name / "lib"
    # The headers come from the Mesa source checkout (`just build-mesa`) or from the
    # published artifact (`just fetch-mesa`), which carries the EGL/GLES2/KHR subset the
    # client compiles against.
    mesa_include = ROOT / "target" / "mesa" / arch.name / "include"
    if not (mesa_include / "EGL" / "egl.h").is_file():
        mesa_include = ROOT / "target" / "mesa-src" / "include"
    if not (libc_dir / LIBC).is_file():
        print(f"error: no {libc_dir / LIBC} — run `just dynlib-{arch.name}` first",
              file=sys.stderr)
        return 1
    if not (mesa_lib / "libEGL.so.1").is_file():
        print(f"error: no {mesa_lib / 'libEGL.so.1'} — run `just build-mesa {arch.name} --stage` "
              f"or `just fetch-mesa {arch.name}`", file=sys.stderr)
        return 1
    if not (mesa_include / "EGL" / "egl.h").is_file():
        print(f"error: no Mesa headers under {mesa_include} — run `just build-mesa "
              f"{arch.name} --build` or `just fetch-mesa {arch.name}`", file=sys.stderr)
        return 1

    work = ROOT / "target" / "mesa" / arch.name / "bin"
    work.mkdir(parents=True, exist_ok=True)
    out = work / "gltriangle"
    scratch = work / "obj"
    scratch.mkdir(parents=True, exist_ok=True)

    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags(),
              f"-I{mesa_include}", "-DEGL_NO_PLATFORM_SPECIFIC_TYPES"]
    crt0 = scratch / "crt0.o"
    obj = scratch / "gl_triangle.o"
    for src, dst in ((arch.crt0, crt0), (SOURCE, obj)):
        if run(["clang", *cflags, "-o", dst, src]) != 0:
            return 1

    stub = scratch / "gltriangle_stub.rs"
    stub.write_text(cdyn.STUB, encoding="utf-8")

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
        "-C", f"link-arg=-L{mesa_lib}",
        "-C", "link-arg=-Bdynamic",
        "-C", "link-arg=--allow-shlib-undefined",
        "-C", f"link-arg=-l:{LIBC}",
        "-C", "link-arg=-l:libEGL.so.1",
        "-C", "link-arg=-l:libGLESv2.so.2",
        "-C", f"link-arg=--dynamic-linker={INTERP}",
        "-o", out,
        stub,
    ]
    if run(link) != 0:
        return 1
    print(f"wrote {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
