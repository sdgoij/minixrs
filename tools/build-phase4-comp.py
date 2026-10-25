#!/usr/bin/env python3
"""Build 4d's gates: smithay's renderer (and, later, the compositor) on the minix targets.

The binaries live in their own cargo project (`tools/phase4-comp`), because they need
smithay with `backend_egl`/`renderer_gl` and the whole `[patch.crates-io]` table — the same
reasoning as `tools/build-phase4-gate.py`, plus one more: the project's `smithay`
dependency is a *path* into `target/smithay`, the pinned checkout
`tools/build-smithay.py` fetches, so that script has to have run before cargo can read the
manifest. This script runs it when the checkout is missing.

Unlike the 4a/4c gates, these binaries are **dynamically linked**: `GlesRenderer` loads
EGL through `libloading`, i.e. `dlopen`, and `minix-libc` answers `NULL` for `dlopen` in a
static image (there is no loader in the address space). So the link carries
`--dynamic-linker=/libexec/ld.so` and a `DT_NEEDED` on `libc.so` and `libEGL.so.1`, with
`libGLESv2.so.2` for the `glGetString` the gate prints and `libxkbcommon` linked
*statically* - a crate's native libraries land inside rustc's `-Bstatic` window.
The entry point stays `minix-rt`'s `_start`, which is what sets up the runtime std expects.

Outputs: `target/<triple>/release/glgate`, where `MINIXFS_EXTRA` can name it.

Usage: python tools/build-phase4-comp.py [all|x86|riscv64|aarch64|<triple>]
                                        (default: x86_64-pc-minix)

Prerequisites: the fork's stage1 compiler (`just bootstrap` or `just fetch-stage1`), an LLD,
`just dynlib-<arch>` for the `libc.so` the image links against, and
`just xkbcommon <arch> --build` for `libxkbcommon.so.0`.

`just glgate-<arch>` is this script; `just test-glgate-<arch>` boots it.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import ALL, Arch, resolve_argv  # noqa: E402
from ccflags import compile_flags  # noqa: E402
from lld import find_lld  # noqa: E402
from stage1 import find_stage1_rustc  # noqa: E402

PROJECT = ROOT / "tools" / "phase4-comp"
BINARY = "glgate"
SMITHAY = ROOT / "target" / "smithay"
# The EGL bootstrap is C (see the file's own note): the context smithay needs cannot come from
# smithay's constructors, and the port's surfaceless recipe is already written in C.
EGL_BOOTSTRAP = PROJECT / "src" / "egl_bootstrap.c"
# Per host: the host artifacts (build scripts, proc macros) are not interchangeable between
# a Windows and a WSL build of the same checkout.
CARGO_TARGET = ROOT / "target" / "phase4-comp"


def die(message: str) -> None:
    sys.exit(f"error: {message}")


def run(cmd: list[str], env: dict[str, str] | None = None) -> int:
    print("running:", " ".join(cmd))
    return subprocess.run(cmd, env=env).returncode


def ensure_smithay(arch: Arch) -> None:
    """Fetch the pinned smithay checkout if it is not there yet.

    `tools/build-smithay.py` owns the pin (and the stage1 capability probe), so it is the
    thing to call rather than duplicating the fetch here.
    """
    if (SMITHAY / ".git").is_dir():
        return
    print("smithay checkout missing: fetching it with tools/build-smithay.py")
    if run([sys.executable, str(ROOT / "tools" / "build-smithay.py"), arch.name]) != 0:
        die("tools/build-smithay.py failed (it fetches target/smithay and checks it)")


def build(arch: Arch, rustc: pathlib.Path, lld: pathlib.Path) -> int:
    script = (ROOT / "tools" / "minix-user.ld").as_posix()
    release = ROOT / "target" / arch.triple / "release"
    xkb_lib = ROOT / "target" / "xkbcommon" / arch.name / "lib"
    mesa_lib = ROOT / "target" / "mesa" / arch.name / "lib"

    for path, why in (
        (release / "libc.so", f"run `just dynlib-{arch.name}` first"),
        (xkb_lib / "libxkbcommon.a",
         f"run `just xkbcommon {arch.name} --build` first"),
        (mesa_lib / "libEGL.so.1",
         f"run `just fetch-mesa {arch.name}` or `just build-mesa {arch.name} --build` first"),
    ):
        if not path.is_file():
            die(f"no {path} - {why}")

    # The Mesa headers the C bootstrap compiles against: the fetched artifact's, or the source
    # checkout's when only `just build-mesa` has run.
    mesa_include = ROOT / "target" / "mesa" / arch.name / "include"
    if not (mesa_include / "EGL" / "egl.h").is_file():
        mesa_include = ROOT / "target" / "mesa-src" / "include"
    if not (mesa_include / "EGL" / "egl.h").is_file():
        die("no EGL headers - run `just fetch-mesa "
            f"{arch.name}` or `just build-mesa {arch.name} --build` first")

    object_dir = CARGO_TARGET / arch.triple / "release"
    object_dir.mkdir(parents=True, exist_ok=True)
    bootstrap_obj = object_dir / "egl_bootstrap.o"
    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags(),
              f"-I{mesa_include}", "-DEGL_NO_PLATFORM_SPECIFIC_TYPES"]
    print("running:", "clang", *cflags, "-o", str(bootstrap_obj), str(EGL_BOOTSTRAP))
    if subprocess.run(["clang", *cflags, "-o", str(bootstrap_obj), str(EGL_BOOTSTRAP)]).returncode != 0:
        return 1

    key = arch.triple.upper().replace("-", "_")
    link = [
        f"-C link-arg=-T{script}",
        "-C link-arg=--no-eh-frame-hdr",
        # The library search path is a *linker* argument: rustc's own crate search is not
        # involved in finding a C shared object.
        f"-C link-arg=-L{release.as_posix()}",
        f"-C link-arg=-L{xkb_lib.as_posix()}",
        "-C link-arg=-Bdynamic",
        "-C link-arg=--allow-shlib-undefined",
        # The C bootstrap's object, which names the `egl*` entry points `libEGL.so.1` defines.
        f"-C link-arg={bootstrap_obj.as_posix()}",
        "-C link-arg=-l:libc.so",
        "-C link-arg=-l:libEGL.so.1",
        # `glGetString`, which the bootstrap calls while the context is current: the driver
        # name is what makes a PASS line say which GL it passed on.
        "-C link-arg=-l:libGLESv2.so.2",
        "-C link-arg=--dynamic-linker=/libexec/ld.so",
    ]
    env = {
        **os.environ,
        "RUSTC": str(rustc),
        "CARGO_TARGET_DIR": str(CARGO_TARGET),
        # `RUSTFLAGS` replaces what the root `.cargo/config.toml` sets, which names
        # `tools/minix-raw.ld` relative to the repo root — and rustc runs the linker from the
        # crate's own directory, so that path does not resolve there. Ours is absolute.
        "RUSTFLAGS": f'--cfg getrandom_backend="custom" '
                     # `-Z pre-link-arg`, not `-C link-arg`: rustc appends `link-arg`s
                     # *after* the crates' own `-l` flags, and `-lxkbcommon` (which the
                     # `xkbcommon` crate emits) is resolved where lld meets it. A search
                     # path added at the end cannot serve it.
                     f"-Z pre-link-arg=-L{xkb_lib.as_posix()} "
                     f"-Z pre-link-arg=-L{release.as_posix()} "
                     f"-Z pre-link-arg=-L{mesa_lib.as_posix()} "
                     + " ".join(link),
        f"CARGO_TARGET_{key}_LINKER": str(lld),
    }
    cmd = [
        "cargo", "build",
        "--manifest-path", str(PROJECT / "Cargo.toml"),
        "--target", arch.triple,
        "--release",
        "--bin", BINARY,
    ]
    if run(cmd, env=env) != 0:
        return 1

    built = CARGO_TARGET / arch.triple / "release" / BINARY
    if not built.is_file():
        print(f"error: {built} was not produced", file=sys.stderr)
        return 1
    dest = release / BINARY
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(built, dest)
    print(f"wrote {dest.relative_to(ROOT)}")
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
        die("the fork's stage1 compiler was not found for this host - run "
            "`just bootstrap` or `just fetch-stage1`")
    lld = find_lld()
    if lld is None:
        die("no lld to link with - run `just bootstrap` or install LLVM")

    for arch in arches:
        ensure_smithay(arch)
        if build(arch, rustc, lld) != 0:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
