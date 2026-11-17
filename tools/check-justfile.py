#!/usr/bin/env python3
"""Reject a Justfile recipe that builds `kernel-boot` without its image inputs.

`crates/kernel/build.rs` reads *every* `BOOT_BINS` entry out of
`target/<triple>/release/` and panics if one is missing:

    kernel build.rs: boot binary .../release/coreutils missing (...)
        the uutils multicall is built from the coreutils submodule

So a recipe that builds `kernel-boot` has to depend on the recipes that produce
them: `userland-<arch>`, `coreutils-<arch>` and `dynlib-<arch>`. A developer's
`target/` usually has them all from an earlier run, so a missing prerequisite is
invisible until a *clean* runner builds that one recipe. That is how CI's
`gltriangle (aarch64)` job failed: `boot-image-aarch64` never depended on
`coreutils-aarch64` after aarch64 started embedding the multicall, while every
local gate had the binary already built by `build-aarch64`.

Exit status is 1 with one line per offender, so `just check` fails on it.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
JUSTFILE = ROOT / "Justfile"
LABEL = "Justfile"

# Cargo's `--target` value to the arch suffix the recipes use.
TARGETS = {
    "x86_64-unknown-minix": "x86",
    "riscv64gc-unknown-minix": "riscv64",
    "aarch64-unknown-minix": "aarch64",
}

# The producers of `manifest::BOOT_BINS`, named `<producer>-<arch>` in the Justfile.
PREREQUISITES = ("userland", "coreutils", "dynlib")

RECIPE = re.compile(r"^([A-Za-z0-9_-]+)(?:[ \t][^:]*)?:(.*)$")
BUILDS_KERNEL_BOOT = re.compile(r"cargo build -p kernel-boot\b")
TARGET = re.compile(r"--target (\S+)")


def walk(lines):
    """Yield `(name, line_number, dependencies, body_lines)` for every recipe."""
    name, number, deps, body = None, 0, [], []
    for index, line in enumerate(lines, 1):
        if line.strip() and not line[0].isspace():
            if name is not None:
                yield name, number, deps, body
            name, deps, body = None, [], []
            m = RECIPE.match(line)
            if m:
                name, number, deps = m.group(1), index, m.group(2).split()
            continue
        if name is not None:
            body.append(line)
    if name is not None:
        yield name, number, deps, body


def main() -> int:
    lines = JUSTFILE.read_text(encoding="utf-8").splitlines()
    problems = []
    for name, number, deps, body in walk(lines):
        arches = set()
        for line in body:
            if not BUILDS_KERNEL_BOOT.search(line):
                continue
            for target in TARGET.findall(line):
                arch = TARGETS.get(target)
                if arch is None:
                    problems.append(
                        f"{LABEL}:{number}: recipe `{name}` builds kernel-boot for "
                        f"unknown target `{target}`; add it to this checker"
                    )
                    continue
                arches.add(arch)
        for arch in sorted(arches):
            for producer in PREREQUISITES:
                want = f"{producer}-{arch}"
                if want not in deps:
                    problems.append(
                        f"{LABEL}:{number}: recipe `{name}` builds kernel-boot for "
                        f"{arch} but does not depend on `{want}` (build.rs embeds it)"
                    )

    for problem in problems:
        print(problem, file=sys.stderr)
    if problems:
        return 1
    print("justfile: every recipe that builds kernel-boot depends on its image inputs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
