#!/usr/bin/env python3
"""A `cc` for x86_64-pc-minix, for C projects that drive a compiler themselves.

bash is one: `configure` compiles and links probe programs, `make` invokes the
compiler once per object and again to link the shell. This is the command those
steps reach, assembled from the two halves the port already has:

  * **compile** — clang with the port's headers, hermetic and freestanding.
    `tools/ccflags.py` has why none of those flags is optional.
  * **link** — the fork's stage1 rustc with `tools/crt0-x86_64.S` and the
    minix-libc rlib, which is the same pair `tools/build-c-hello.py` assembles
    by hand for `/bin/helloc`.

Point a build at it by name, exactly as a cross build would:

    CC="python3 <repo>/tools/cc-minix.py"

`tools/build-bash.py` does that. The build host has to be POSIX (the rlib, the
stage1 and clang must be the same host's): see `C_BUILD.md`.
"""

from __future__ import annotations

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

from ccflags import compile_flags, link_passthrough  # noqa: E402
from lld import find_lld, host_triple  # noqa: E402

TARGET = "x86_64-pc-minix"
CRT0_S = ROOT / "tools" / "crt0-x86_64.S"
LD_SCRIPT = ROOT / "tools" / "minix-user.ld"
WORK = ROOT / "target" / "cc-minix"
LIBC_DEPS = ROOT / "target" / TARGET / "release" / "deps"

# `--target=x86_64-unknown-none` is what tools/c-include's headers are written
# for; the minix triple is what the rlib and crt0 are built for. They are the
# same machine, and the link step below is what makes that explicit.
BASE_CFLAGS = [
    "--target=x86_64-unknown-none",
    "-ffreestanding",
    "-mno-red-zone",
    "-fno-stack-protector",
    "-fno-pic",
]

# The rustc link wants a crate root; this one pulls the libc in and is otherwise
# empty, the same stub tools/build-c-hello.py writes.
STUB = """#![no_std]
#![no_main]
extern crate minix_libc;
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! { loop {} }
"""

# Arguments taking a separate value. Everything else that starts with `-` is a
# flag; the inputs are what is left. A wrapper that gets this wrong either drops
# an input or hands the compiler an argument that belongs to the link.
VALUE_FLAGS = frozenset({
    "-o", "-I", "-D", "-U", "-L", "-l", "-isystem", "-include", "-imacros",
    "-MF", "-MT", "-MQ", "-x", "--param", "-std", "-Xlinker", "-u",
})

# `configure` and `make` ask the compiler what it is. A wrapper that fails these
# is rejected as "not working", so they answer without compiling anything.
VERSION_PROBES = frozenset({
    "--version", "-v", "-V", "-qversion", "-dumpversion", "-dumpmachine", "--help",
})
COMPILE_ONLY = frozenset({"-c", "-E", "-S", "-M", "-MM", "-fsyntax-only"})


def find_stage1_rustc() -> pathlib.Path | None:
    """The stage1 rustc for *this* host.

    Named by the host triple rather than globbed: the tree holds a stage1 per
    host that ever built here (`rust/build/host` is the Windows one, and
    `rust/build/x86_64-pc-windows-msvc` another), and a compiler for a different
    host produces rlibs and binaries this one cannot link.
    """
    override = os.environ.get("MINIXRS_STAGE1_RUSTC")
    if override:
        return pathlib.Path(override)
    host = host_triple()
    if not host:
        return None
    for name in ("rustc", "rustc.exe"):
        exe = ROOT / "rust" / "build" / host / "stage1" / "bin" / name
        if exe.is_file():
            return exe
    return None


def newest_libc_rlib() -> pathlib.Path | None:
    """The minix-libc rlib to link against.

    Newest by mtime, because more than one can sit in `deps/`: the rlib has two
    builders (the Windows and the Linux stage1) and cargo names each build by the
    metadata it was given, so a tree that has built on both hosts holds two.
    `tools/build-bash.py` deletes the stale one and rebuilds with this host's
    stage1, which is what makes "newest" the right one to take.
    """
    rlibs = list(LIBC_DEPS.glob("libminix_libc-*.rlib"))
    return max(rlibs, key=lambda p: p.stat().st_mtime) if rlibs else None


def run(cmd: list[str]) -> int:
    print("+", " ".join(cmd), file=sys.stderr)
    return subprocess.run(cmd).returncode


def split_args(argv: list[str]) -> tuple[list[str], list[str], str | None]:
    """(flags, inputs, -o value) for one `cc` line."""
    flags: list[str] = []
    inputs: list[str] = []
    out: str | None = None
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "-o":
            i += 1
            out = argv[i] if i < len(argv) else None
        elif arg in VALUE_FLAGS:
            flags.append(arg)
            if i + 1 < len(argv):
                flags.append(argv[i + 1])
                i += 1
        elif arg.startswith("-"):
            flags.append(arg)
        else:
            inputs.append(arg)
        i += 1
    return flags, inputs, out


def main(argv: list[str]) -> int:
    if not argv:
        print("cc-minix: no arguments", file=sys.stderr)
        return 1
    if len(argv) == 1 and argv[0] in VERSION_PROBES:
        print("cc-minix: clang for x86_64-unknown-none, linked by the minix "
              "fork's stage1 rustc")
        return 0

    rustc = find_stage1_rustc()
    if rustc is None:
        host = host_triple() or "<unknown>"
        print(f"cc-minix: no stage1 rustc at rust/build/{host}/stage1/bin — "
              "run `just fetch-stage1` (or bootstrap) on this host", file=sys.stderr)
        return 1
    lld = find_lld()
    if lld is None:
        print("cc-minix: no lld to link with (see tools/lld.py)", file=sys.stderr)
        return 1

    flags, inputs, out = split_args(argv)
    WORK.mkdir(parents=True, exist_ok=True)

    if any(a in COMPILE_ONLY for a in argv):
        cmd = ["clang", *BASE_CFLAGS, *compile_flags(), *flags, *inputs]
        if out:
            cmd += ["-o", out]
        return run(cmd)

    if not inputs:
        print("cc-minix: no input files", file=sys.stderr)
        return 1

    # Link: compile the C/C++ inputs first, keep archives and objects as they
    # are, then hand the whole set to rustc along with crt0 and the rlib.
    objects: list[str] = []
    for src in inputs:
        if src.endswith((".o", ".a")):
            objects.append(src)
            continue
        obj = WORK / (pathlib.Path(src).stem + ".o")
        if run(["clang", *BASE_CFLAGS, *compile_flags(), *flags, "-c", src,
                "-o", str(obj)]) != 0:
            return 1
        objects.append(str(obj))

    crt0 = WORK / "crt0.o"
    if run(["clang", *BASE_CFLAGS, "-c", str(CRT0_S), "-o", str(crt0)]) != 0:
        return 1

    rlib = newest_libc_rlib()
    if rlib is None:
        print("cc-minix: no minix-libc rlib — run `just build-bash` (it builds one "
              "with this host's stage1) or `cargo build -p minix-libc --target "
              f"{TARGET} --release`", file=sys.stderr)
        return 1

    stub = WORK / "link_stub.rs"
    stub.write_text(STUB, encoding="utf-8")

    cmd = [str(rustc), "--crate-type", "bin", "--target", TARGET, "--edition", "2024",
           "-C", f"link-arg=-T{LD_SCRIPT}", "-C", f"linker={lld}",
           "-C", f"link-arg={crt0}"]
    for obj in objects:
        cmd += ["-C", f"link-arg={obj}"]
    # `-l`/`-L`/`-Wl,` belong to the link, not the compile: without them the
    # executable has no archives in it and every symbol in them is undefined.
    for arg in link_passthrough(argv):
        cmd += ["-C", f"link-arg={arg}"]
    cmd += ["--extern", f"minix_libc={rlib}", "-L", f"dependency={LIBC_DEPS}",
            "-o", out if out else "a.out", str(stub)]
    return run(cmd)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
