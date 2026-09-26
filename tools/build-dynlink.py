#!/usr/bin/env python3
"""Build the Phase 0 dynamic-linking artifacts.

Produces, under `target/dynlink/`:

* `libdyn.so`, `libdyn2.so` — the shared objects (`tools/libdyn.c`,
  `tools/libdyn2.c`), built with LLD directly;
* `dynhello`   — a classic-dynamic `ET_EXEC` (`tools/dynhello.c`) with `PT_INTERP`
  (`/libexec/ld.so`) and `DT_NEEDED` for both objects.

The executable is linked with the fork's stage1 rustc as the driver (the same way
`tools/build-c-hello.py` does it), because its `write`/`exit` come from the
`minix-libc` rlib; the C objects and the shared objects are clang + LLD's work.

Phase 0 was one object; Phase 1 needs two, because the loader's *base* allocator
is only exercised when a second object has to land somewhere other than the
first. Phase 2 chains them: `libdyn` names `libdyn2`, so one of the objects is a
dependency of a dependency and of the executable at the same time.

Phase 7 made this three-arch: the C objects and the executable are compiled and
linked for whichever target is named, and only the loader's own `_start` and TLS
placement differ between them (`crates/ldso`).

Usage: python tools/build-dynlink.py [x86|riscv64|aarch64]

Prerequisites: the fork's stage1 compiler and an LLD (`just bootstrap`), and
clang on PATH.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "tools"))

from ccarch import X86_64, Arch, resolve_argv  # noqa: E402
from ccflags import compile_flags  # noqa: E402
from lld import find_lld  # noqa: E402

DSOS = ("libdyn2", "libdyn")
INTERP = "/libexec/ld.so"
OUT = ROOT / "target" / "dynlink"


def find_stage1_rustc() -> "pathlib.Path | None":
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
    work = OUT / arch.name
    work.mkdir(parents=True, exist_ok=True)

    cflags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-c", *compile_flags()]

    # crt0 and the program are non-PIC (a non-PIE executable, per the OS's
    # linker script).
    if run(["clang", *cflags, "-o", work / "crt0.o", arch.crt0]) != 0:
        return 1
    if run(["clang", *cflags, "-o", work / "dynhello.o", ROOT / "tools" / "dynhello.c"]) != 0:
        return 1

    # The shared objects must be position-independent. `libdyn2` is built and linked
    # first: `libdyn` names it as its own DT_NEEDED, which is what makes the loader
    # follow a dependency of a dependency (and find the object the executable also
    # names, so it is mapped once).
    dso_flags = [*arch.base_cflags(), "-fno-builtin", "-O2", "-fPIC", "-c", *compile_flags()]
    for name in DSOS:
        if run(["clang", *dso_flags, "-o", work / f"{name}.o", ROOT / "tools" / f"{name}.c"]) != 0:
            return 1

    # Link each .so with LLD directly (the minix target specs ask for `lld` on
    # PATH; we have the toolchain's own), against the objects built before it.
    for i, name in enumerate(DSOS):
        dso = work / f"{name}.so"
        earlier = [arg for dep in DSOS[:i] for arg in (f"-L{work}", f"-l:{dep}.so")]
        link_so = [lld, "-flavor", "gnu", "-shared", "-soname", f"{name}.so", "-o", dso,
                   work / f"{name}.o", *earlier]
        if run(link_so) != 0:
            return 1
        print(f"wrote {dso}")

    # minix-libc for the target: the executable's write/exit come from here.
    env = {**os.environ, "RUSTC": str(rustc)}
    if run(["cargo", "build", "-p", "minix-libc", "--target", arch.triple, "--release"], env=env) != 0:
        return 1
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

    out = work / "dynhello"
    # Each shared object the executable names, as a pair of `-C link-arg` entries.
    libs = [arg for name in DSOS for arg in ("-C", f"link-arg=-l:{name}.so")]
    link = [
        rustc,
        "--crate-type", "bin",
        "--target", arch.triple,
        "--edition", "2024",
        "-C", f"link-arg=-T{ROOT / 'tools' / 'minix-user.ld'}",
        "-C", f"linker={lld}",
        "-C", f"link-arg={work / 'crt0.o'}",
        "-C", f"link-arg={work / 'dynhello.o'}",
        # Resolve the calls and the data symbol against the shared objects, and
        # record the interpreter that must run before `main`. `-Bdynamic` is
        # needed because the minix target's `crt_static_default` makes rustc pass
        # `-static`, under which lld refuses to link a `.so` at all.
        "-C", f"link-arg=-L{work}",
        "-C", "link-arg=-Bdynamic",
        *libs,
        "-C", f"link-arg=--dynamic-linker={INTERP}",
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
    arch, rest = resolve_argv(argv)
    if rest:
        sys.exit(f"error: unknown argument {rest[0]!r}")

    rustc = find_stage1_rustc()
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
