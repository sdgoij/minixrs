#!/usr/bin/env python3
"""Build `/bin/wldmabufgl` — the linux-dmabuf GL present client (.notes/wayland.md §6.10,
stage 3d-b-4d).

The same shape `tools/build-wlgl.py` builds, and for the same reason: `dlopen` needs a
loader in the address space, and a statically linked minix image has none, so a program
that renders with Mesa must be a dynamic image. What differs is the linked DSOs — EGL and
GLESv2 for the render, `libgbm` for the buffer object, `libdrm` under it — and the C object,
which draws into a GBM BO and returns its dma-buf instead of reading a framebuffer back.

Usage: python tools/build-wldmabufgl.py [x86]
Writes `target/mesa/<arch>/bin/wldmabufgl`.
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
RENDER = ROOT / "tools" / "wldmabufgl_render.c"

STUB = '''#![no_std]
#![no_main]

// Written by tools/build-wldmabufgl.py.

#[unsafe(no_mangle)]
pub extern "C" fn main(argc: i32, argv: *const *const u8, _envp: *const *const u8) -> i32 {
    let mut buf = [""; 64];
    let args = unsafe { userland::parse_args(argc, argv, &mut buf) };
    userland::wlclient::wl_dmabuf_gl(args)
}
'''

# The C crt0's entry symbol, renamed at compile time: `minix-rt` defines `_start` too (its
# own runtime's entry, pulled in with `minix-std`), and two definitions do not link.
CRT0_ENTRY = "minix_crt0_entry"


def run(cmd: list[object], env: "dict | None" = None) -> int:
    print("running:", " ".join(str(c) for c in cmd))
    return subprocess.run([str(c) for c in cmd], env=env).returncode


def build_rlib(arch, rustc) -> int:
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
    mesa_src = ROOT / "target" / "mesa-src"
    drm_src = ROOT / "target" / "drm-src"

    for path, why in (
        (userland, "run `just build-x86` first, which builds it"),
        (release / LIBC, f"run `just dynlib-{arch.name}` first"),
        (mesa_lib / "libEGL.so.1",
         f"run `just build-mesa {arch.name} --build`, or `just fetch-mesa {arch.name}`"),
        (mesa_lib / "libgbm.so.1",
         f"run `just build-mesa {arch.name} --build` (GBM is enabled in the build config)"),
        (mesa_src / "include" / "EGL" / "eglext.h",
         f"run `just build-mesa {arch.name} --build` (the client compiles against the sources)"),
    ):
        if not path.is_file():
            print(f"error: no {path} — {why}", file=sys.stderr)
            return 1

    work = ROOT / "target" / "mesa" / arch.name / "bin"
    work.mkdir(parents=True, exist_ok=True)
    out = work / "wldmabufgl"
    scratch = work / "obj"
    scratch.mkdir(parents=True, exist_ok=True)

    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags(),
              f"-I{mesa_src / 'include'}", f"-I{mesa_src / 'src' / 'gbm' / 'main'}",
              f"-I{drm_src}", f"-I{drm_src / 'include' / 'drm'}",
              "-DEGL_NO_PLATFORM_SPECIFIC_TYPES"]
    crt0 = scratch / "crt0.o"
    obj = scratch / "wldmabufgl_render.o"
    if run(["clang", *cflags, f"-D_start={CRT0_ENTRY}", "-o", crt0, arch.crt0]) != 0:
        return 1
    if run(["clang", *cflags, "-o", obj, RENDER]) != 0:
        return 1

    stub = scratch / "wldmabufgl_stub.rs"
    stub.write_text(STUB, encoding="utf-8")

    link = [
        rustc,
        "--crate-type", "bin",
        "--target", arch.triple,
        "--edition", "2024",
        "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
        "-C", f"linker={lld}",
        "-C", "link-arg=-e",
        "-C", f"link-arg={CRT0_ENTRY}",
        "-C", f"link-arg={crt0}",
        "-C", f"link-arg={obj}",
        "-C", f"link-arg=-L{release}",
        "-C", f"link-arg=-L{mesa_lib}",
        "-L", f"dependency={arch.libc_deps}",
        "-C", "link-arg=-Bdynamic",
        "-C", "link-arg=--allow-shlib-undefined",
        "-C", f"link-arg=-l:{LIBC}",
        "-C", "link-arg=-l:libEGL.so.1",
        "-C", "link-arg=-l:libGLESv2.so.2",
        "-C", "link-arg=-l:libgbm.so.1",
        "-C", "link-arg=-l:libdrm.so.2",
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
