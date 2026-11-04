#!/usr/bin/env python3
"""List every repository in this tree whose commits are not on its remote.

The superproject and every registered submodule are checked, so a fork that was
left behind in a push shows up here instead of being discovered later. Each
repository is compared with its own upstream branch; nothing is fetched unless
`--fetch` is given, so without it the answer is only as fresh as the last fetch.

Exit status is 1 when something needs a push (or a tree is dirty), so a release
gate can call this.

Usage:
    python tools/push-status.py            # report, no network
    python tools/push-status.py --fetch    # refresh remote-tracking refs first
    python tools/push-status.py --commits  # also list the unpushed subjects
"""

import argparse
import subprocess
import sys


def git(repo, *args):
    """Run git in `repo` and return stdout, or None if it failed."""
    proc = subprocess.run(
        ["git", "-C", repo, *args],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        return None
    return proc.stdout.strip()


def repositories(root):
    """The superproject plus every top-level submodule path.

    Not recursive on purpose: a submodule's own submodules (the vendored
    `llvm-project`, `backtrace`, the Wayland protocol trees) belong to its
    upstream and are never something this project pushes.
    """
    paths = ["."]
    listing = git(root, "submodule", "status") or ""
    for line in listing.splitlines():
        # One line per submodule: a status character, the recorded commit, the
        # path, and an optional `(describe)` tail. A leading `-` means the
        # submodule is not checked out, so there is nothing to compare.
        if not line or line[0] == "-":
            continue
        fields = line[1:].split()
        if len(fields) >= 2:
            paths.append(fields[1])
    return paths


def describe(repo):
    """Everything the report needs about one repository."""
    branch = git(repo, "rev-parse", "--abbrev-ref", "HEAD") or "?"
    upstream = git(repo, "rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}")
    ahead = behind = None
    if upstream:
        counts = git(repo, "rev-list", "--left-right", "--count", f"{upstream}...HEAD")
        if counts:
            behind_str, ahead_str = counts.split()
            ahead, behind = int(ahead_str), int(behind_str)
    else:
        # No upstream: a detached vendored checkout (llvm-project, the Wayland
        # protocol trees) still has an answer, namely how many of its commits no
        # remote-tracking ref reaches.
        unreachable = git(repo, "rev-list", "--count", "HEAD", "--not", "--remotes")
        if unreachable is not None:
            ahead = int(unreachable)
    dirty = bool(git(repo, "status", "--porcelain"))
    return {
        "branch": branch,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
        "dirty": dirty,
    }


def unpushed_subjects(repo):
    subjects = git(repo, "log", "--oneline", "--no-decorate", "@{upstream}..HEAD") or ""
    return [line for line in subjects.splitlines() if line]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetch", action="store_true", help="fetch every repository first")
    parser.add_argument("--commits", action="store_true", help="list the unpushed commits")
    args = parser.parse_args()

    paths = repositories(".")
    if args.fetch:
        for path in paths:
            git(path, "fetch", "--quiet", "--prune")

    rows = []
    needs_push = []
    dirty = []
    for path in paths:
        info = describe(path)
        rows.append((path, info))
        if info["ahead"]:
            needs_push.append((path, info))
        if info["dirty"]:
            dirty.append(path)

    width = max(len(path) for path, _ in rows) + 2
    print(f"{'repo':<{width}}{'branch':<24}{'ahead':>6}{'behind':>7}  tree")
    for path, info in rows:
        ahead = "-" if info["ahead"] is None else str(info["ahead"])
        behind = "-" if info["behind"] is None else str(info["behind"])
        tree = "dirty" if info["dirty"] else "clean"
        upstream = "" if info["upstream"] else "  (no upstream)"
        print(f"{path:<{width}}{info['branch']:<24}{ahead:>6}{behind:>7}  {tree}{upstream}")

    if not args.fetch:
        print("\nremote-tracking refs are as of the last fetch; pass --fetch to refresh them")

    if needs_push:
        print("\nneeds a push:")
        for path, info in needs_push:
            print(f"  {path} ({info['branch']}): {info['ahead']} commit(s)")
            if args.commits:
                for subject in unpushed_subjects(path):
                    print(f"      {subject}")
    else:
        print("\nnothing to push")

    if dirty:
        print("\ndirty working trees:")
        for path in dirty:
            print(f"  {path}")

    return 1 if needs_push or dirty else 0


if __name__ == "__main__":
    sys.exit(main())
