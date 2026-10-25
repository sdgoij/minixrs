#!/usr/bin/env python3
"""Build the Phase 4 gates: `calloopgate` (4a), `wlgate` (4c) and the `udsprobe` diagnostic.

The gate binaries are a cargo project of its own (`tools/phase4-gate`), because they need the
forked OS crates and `[patch.crates-io]` is only legal at a workspace root — the same shape as
`tools/build-smithay.py`, and for the same reason: the root workspace must not read that patch
table, or the forks would become a checkout requirement for every build in the tree.

Outputs: `target/<triple>/release/{calloopgate,wlgate,udsprobe}`, where `MINIXFS_EXTRA` can name
them.

Usage: python tools/build-phase4-gate.py [all|x86|riscv64|aarch64|<triple>]
                                        (default: x86_64-pc-minix)

Prerequisites: the fork's stage1 compiler with this tree's std (`just bootstrap` or
`just fetch-stage1`), and an LLD (`just bootstrap`, or a system LLVM).

`just calloop-<arch>` is this script; `just test-calloop-<arch>` boots it.
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
from lld import find_lld  # noqa: E402
from stage1 import find_stage1_rustc  # noqa: E402

PROJECT = ROOT / "tools" / "phase4-gate"
# The gate binaries this script produces; each is copied to `target/<triple>/release/`.
BINARIES = ("calloopgate", "wlgate", "udsprobe")
# Per host: the host artifacts (build scripts, proc macros) are not interchangeable between a
# Windows and a WSL build of the same checkout.
CARGO_TARGET = ROOT / "target" / "phase4-gate"


def build(arch: Arch, rustc: pathlib.Path, lld: pathlib.Path) -> int:
    script = (ROOT / "tools" / "minix-user.ld").as_posix()
    key = arch.triple.upper().replace("-", "_")
    env = {
        **os.environ,
        "RUSTC": str(rustc),
        "CARGO_TARGET_DIR": str(CARGO_TARGET),
        # `RUSTFLAGS`, not `CARGO_TARGET_<triple>_RUSTFLAGS`: the env var *replaces* the
        # rustflags the root `.cargo/config.toml` sets, which name `tools/minix-raw.ld`
        # relative to the repo root — and rustc runs the linker from the crate's own
        # directory, so that path does not resolve there. Ours is absolute.
        "RUSTFLAGS": f"-C link-arg=-T{script} -C link-arg=--no-eh-frame-hdr",
        f"CARGO_TARGET_{key}_LINKER": str(lld),
    }
    cmd = [
        "cargo", "build",
        "--manifest-path", str(PROJECT / "Cargo.toml"),
        "--target", arch.triple,
        "--release",
    ]
    for b in BINARIES:
        cmd += ["--bin", b]
    print("running:", " ".join(cmd))
    if subprocess.run(cmd, env=env).returncode != 0:
        return 1

    for b in BINARIES:
        built = CARGO_TARGET / arch.triple / "release" / b
        if not built.is_file():
            print(f"error: {built} was not produced", file=sys.stderr)
            return 1
        dest = ROOT / "target" / arch.triple / "release" / b
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
        print(
            "error: the fork's stage1 compiler was not found for this host — run "
            "`just bootstrap` or `just fetch-stage1`",
            file=sys.stderr,
        )
        return 1
    lld = find_lld()
    if lld is None:
        print(
            "error: no lld to link with — run `just bootstrap` or install LLVM",
            file=sys.stderr,
        )
        return 1

    for arch in arches:
        if build(arch, rustc, lld) != 0:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
