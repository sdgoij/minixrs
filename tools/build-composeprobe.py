#!/usr/bin/env python3
"""Build the 3d-b-5 GPU-composition probe (§6.10 3d-b-5).

The same shape `tools/build-eglprobe.py` builds — a dynamic minix image with the arch's C
crt0, `minix-user.ld`, and `-Bdynamic` against EGL, GLESv2, GBM and libdrm. It includes the
headers from the Mesa and libdrm source checkouts (`just build-mesa`).

Usage: python tools/build-composeprobe.py [x86]
Writes `target/mesa/<arch>/bin/composeprobe`.
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
SOURCE = ROOT / "tools" / "compose_probe.c"


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
    mesa_src = ROOT / "target" / "mesa-src"
    drm_src = ROOT / "target" / "drm-src"

    if not (libc_dir / LIBC).is_file():
        print(f"error: no {libc_dir / LIBC} — run `just dynlib-{arch.name}` first",
              file=sys.stderr)
        return 1
    for lib in ("libEGL.so.1", "libGLESv2.so.2", "libgbm.so.1"):
        if not (mesa_lib / lib).is_file():
            print(f"error: no {mesa_lib / lib} — run `just build-mesa {arch.name} --build`",
                  file=sys.stderr)
            return 1
    for header in (mesa_src / "include" / "EGL" / "eglext.h",
                   mesa_src / "include" / "GLES2" / "gl2.h",
                   mesa_src / "src" / "gbm" / "main" / "gbm.h",
                   drm_src / "xf86drm.h"):
        if not header.is_file():
            print(f"error: no {header} — run `just build-mesa {arch.name} --build`",
                  file=sys.stderr)
            return 1

    work = ROOT / "target" / "mesa" / arch.name / "bin"
    work.mkdir(parents=True, exist_ok=True)
    out = work / "composeprobe"
    scratch = work / "obj"
    scratch.mkdir(parents=True, exist_ok=True)

    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags(),
              f"-I{mesa_src / 'include'}", f"-I{mesa_src / 'src' / 'gbm' / 'main'}",
              f"-I{drm_src}", f"-I{drm_src / 'include' / 'drm'}",
              "-DEGL_NO_PLATFORM_SPECIFIC_TYPES"]
    crt0 = scratch / "crt0.o"
    obj = scratch / "compose_probe.o"
    for src, dst in ((arch.crt0, crt0), (SOURCE, obj)):
        if run(["clang", *cflags, "-o", dst, src]) != 0:
            return 1

    stub = scratch / "composeprobe_stub.rs"
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
        "-C", "link-arg=-l:libgbm.so.1",
        "-C", "link-arg=-l:libdrm.so.2",
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
