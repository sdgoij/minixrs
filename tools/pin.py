#!/usr/bin/env python3
"""The toolchain pin an artifact is built against, read out of the tree.

`rust` is a submodule, so the commit it pins is a *gitlink* in the parent
repository — the value `git rev-parse HEAD:rust` prints. That commit belongs in a
release tag beside the third-party version, because a fork bump can change the
target ABI and the C++ headers: a shell or a DSO built with the old toolchain
must not be handed out for the new one, and the pin is what makes that automatic.

Reading the gitlink rather than `git -C rust rev-parse HEAD` is deliberate: it
works whether or not the submodule is checked out, which is what lets a read-only
`*-pin` job resolve a tag without fetching the submodule's ~4.5 GiB of history.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]


def rust_gitlink() -> str:
    """The commit the `rust` submodule pins, or a die naming the failure."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD:rust"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (subprocess.CalledProcessError, FileNotFoundError) as e:
        sys.exit(f"error: cannot read the pinned `rust` commit (`git rev-parse HEAD:rust`): {e}")
    if len(out) < 12:
        sys.exit(f"error: `HEAD:rust` is not a commit: {out!r}")
    return out
