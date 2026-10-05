#!/usr/bin/env python3
"""Create or refresh the `sdgoij/bash` GitHub mirror the build clones from.

Usage: python tools/mirror-bash.py [--check]

savannah is slow and times out, so the build does not clone from it: CI and dev
machines clone [`BASH_GIT`], a mirror in this project's own GitHub org
(`tools/bash_pin.py`). This tool is how that mirror is created and refreshed — a
rare, local step, which is the point: the flaky fetch never sits on a CI critical
path.

The objects are taken from [`BASH_UPSTREAM`] (savannah), never from another mirror
of unknown provenance, so this project is the anchor for what the mirror holds. The
pinned commit is verified present before anything is pushed, so a pin that upstream
has not published cannot be mirrored by accident.

  create or refresh, then push:   python tools/mirror-bash.py
  only verify the pin is present: python tools/mirror-bash.py --check

`MINIXRS_BASH_UPSTREAM`, `MINIXRS_BASH_PUSH` and `MINIXRS_BASH_RETRIES` override the
upstream URL, the push URL and the fetch attempt count.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

from bash_pin import (  # noqa: E402
    BASH_COMMIT,
    BASH_DESCRIBE,
    BASH_GIT,
    BASH_PUSH,
    BASH_UPSTREAM,
)

# A bare mirror under `target/`, beside the build's own checkout, so neither is in
# the tree and both are taken by `cargo clean` / `just bootstrap` together.
MIRROR = ROOT / "target" / "bash-mirror.git"


def note(msg: str) -> None:
    print(f"[bash] {msg}", flush=True)


def die(msg: str):
    sys.exit(f"error: {msg}")


def run(cmd: list[str], **kwargs) -> int:
    print("+", " ".join(str(c) for c in cmd), file=sys.stderr, flush=True)
    return subprocess.run([str(c) for c in cmd], **kwargs).returncode


def have_commit(commit: str) -> bool:
    return subprocess.run(
        ["git", "-C", str(MIRROR), "rev-parse", "--verify", "--quiet", f"{commit}^{{commit}}"],
        capture_output=True,
    ).returncode == 0


def refresh(upstream: str, retries: int) -> None:
    """Clone the mirror once, then fetch it, retrying against savannah's timeouts."""
    for attempt in range(1, retries + 1):
        fresh = not MIRROR.is_dir()
        if fresh:
            MIRROR.parent.mkdir(parents=True, exist_ok=True)
            note(f"cloning {upstream} (a full mirror, ~290 MiB the first time)")
            cmd = ["git", "-c", "core.autocrlf=false", "clone", "--mirror",
                   upstream, str(MIRROR)]
        else:
            cmd = ["git", "-C", str(MIRROR), "remote", "update", "--prune"]
        if run(cmd) == 0:
            return
        # A failed clone leaves a half-fetched directory behind; drop it so the next
        # attempt starts clean rather than resuming something unusable.
        if fresh and MIRROR.is_dir():
            shutil.rmtree(MIRROR, ignore_errors=True)
        note(f"attempt {attempt}/{retries} failed")
    die(f"could not refresh the mirror from {upstream} after {retries} attempts "
        "(savannah is slow; retry later, or point MINIXRS_BASH_UPSTREAM at a copy)")


def main(argv: list[str]) -> int:
    check = "--check" in argv
    upstream = os.environ.get("MINIXRS_BASH_UPSTREAM", BASH_UPSTREAM)
    push = os.environ.get("MINIXRS_BASH_PUSH", BASH_PUSH)
    try:
        retries = int(os.environ.get("MINIXRS_BASH_RETRIES", "3"))
    except ValueError:
        die("MINIXRS_BASH_RETRIES must be an integer")

    refresh(upstream, retries)

    if not have_commit(BASH_COMMIT):
        die(f"{BASH_DESCRIBE} ({BASH_COMMIT}) is not in the mirror after refreshing "
            f"{upstream}; it is the commit the build pins, so the mirror is not "
            "usable as it stands")
    note(f"{BASH_DESCRIBE} ({BASH_COMMIT[:12]}) is present")

    if check:
        return 0

    if run(["git", "-C", str(MIRROR), "push", "--mirror", push]) != 0:
        die(f"could not push to {push}; create the GitHub repository empty (no "
            "README, no .gitignore) and make sure this host's key can push to it "
            "(MINIXRS_BASH_PUSH overrides the remote)")
    note(f"pushed to {push}; the build clones {BASH_GIT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
