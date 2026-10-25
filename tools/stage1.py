#!/usr/bin/env python3
"""Locate the fork's stage1 `rustc`.

The stage1 compiler is the one with the minix targets built in and a sysroot for
them, so it is the `RUSTC` of every minix build: the Justfile's `stage1-rustc`,
and the scripts that invoke rustc directly (`tools/build-c-hello.py`,
`tools/build-dynlibc.py` through `tools/cdyn.py`, `tools/build-dynlink.py`,
`tools/build-std-hello.py`, `tools/cc-minix.py`'s wrapper).

Candidates, in order:

1. `MINIXRS_STAGE1_RUSTC` - an explicit override, mainly for the self-test.
2. `rust/build/<host>/stage1/bin/rustc` - x.py's build directory for the host
   running this, the same host `tools/lld.py` resolves its build directories for.

Never another host's, and never `rust/build/host` unless the host triple could
not be detected. A tree shared between Windows and WSL holds a stage1 per host,
and `build/host` on such a tree is whichever host created the symlink. Picking
the other one is not a clean failure: on Linux, WSL interop will happily execute
`rustc.exe`, and it then writes Windows paths into the dependency files - cargo
reports that as an unreadable `.d` file or as `failed to join paths from
$LD_LIBRARY_PATH`.

Usage: python tools/stage1.py
Prints the compiler's path, or nothing when there is none: whether that is fatal
depends on the caller. `just bootstrap` on a host that has not built one yet
still has to be able to parse the Justfile.
"""

from __future__ import annotations

import os
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "rust" / "build"

sys.path.insert(0, str(ROOT / "tools"))

from lld import exe, host_triple  # noqa: E402


def find_stage1_rustc() -> "pathlib.Path | None":
    """The stage1 `rustc` for this host, or None."""
    override = os.environ.get("MINIXRS_STAGE1_RUSTC")
    if override:
        return pathlib.Path(override)
    host = host_triple()
    if not host and not BUILD.is_dir():
        return None
    bin_dir = (BUILD / host if host else BUILD / "host") / "stage1" / "bin"
    rustc = bin_dir / exe("rustc")
    return rustc if rustc.is_file() else None


def main() -> int:
    found = find_stage1_rustc()
    if found is not None:
        # Forward slashes: the Justfile substitutes this into `sh` recipes, and a
        # Windows path is equally valid to cargo and rustc in that spelling.
        print(found.as_posix())
    return 0


if __name__ == "__main__":
    sys.exit(main())
