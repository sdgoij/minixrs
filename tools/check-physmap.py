#!/usr/bin/env python3
"""Reject a physical address used as a pointer without the physmap conversion.

`PHYSMAP.md` D4/P6. A frame's physical address is a number, not a virtual address the kernel may
dereference. `pte_to_phys`/`pte_frame` return one, a masked PTE *is* one, and so is any `*_phys` or
`*_pa` binding the kernel keeps for one. The only sound way from one to a pointer is the
conversion: the HAL's `phys_to_virt` / `PHYS_TO_VIRT`, or `kernel::pagetable`'s `frame_ptr` /
`table_ptr`.

Scope: `crates/kernel/src` and `crates/arch-*/src` — the crates the kernel's own address-space
rules govern. A file's test tail (`#[cfg(test)] mod tests`) is out of scope, because a host fixture
has no physmap and the identity is the only thing that can work there (see `table_ptr`).

A deliberate exception states itself on the line, with its reason:

    let ptr = pa as *mut u8; // physmap-ok: <why the identity is the right view here>

Exit status is 1 with one line per offender, so `just check` fails on it.
"""

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent

# The conversion is what makes a physical address usable as a pointer.
CONVERSIONS = (
    "phys_to_virt",
    "PHYS_TO_VIRT",
    "frame_ptr",
    "table_ptr",
    "phys_ptr",
    "walk_phys_ptr",
)

# A quantity derived from a page-table entry: the masks each arch's HAL uses, and the walk helpers.
TABLES = re.compile(
    r"(pte_to_phys|pte_frame|pte_phys|pte_frame_mask|PG_FRAME|PPN_MASK|PTE_ADDR_MASK|PG_BLOCK_ADDR_MASK)"
)

# A binding whose name says it holds a physical address.
NAMED = re.compile(r"(\w+_phys|\w+_pa|phys_base|\bpa\b|\bphys\b)\s*as\s*\*(?:mut|const)")

CAST = re.compile(r"as\s*\*(?:mut|const)")


def in_scope(path: pathlib.Path) -> bool:
    parts = path.parts
    if "kernel" in parts:
        return "src" in parts and path.suffix == ".rs"
    return any(p.startswith("arch-") for p in parts) and path.suffix == ".rs"


def test_tail(lines: list[str]) -> int:
    """Index of the file's `#[cfg(test)] mod tests`, or len(lines) if it has none."""
    for i, line in enumerate(lines):
        if not line.strip().startswith("#[cfg(test)]"):
            continue
        for j in range(i + 1, min(i + 4, len(lines))):
            if lines[j].strip().startswith("mod tests"):
                return i
    return len(lines)


def offenders() -> list[str]:
    found = []
    for path in sorted(ROOT.glob("crates/*/src/**/*.rs")):
        rel = path.relative_to(ROOT)
        if not in_scope(rel):
            continue
        lines = path.read_text(encoding="utf-8", errors="replace").splitlines()
        for n, line in enumerate(lines[: test_tail(lines)], start=1):
            if "physmap-ok" in line or not CAST.search(line):
                continue
            if any(c in line for c in CONVERSIONS):
                continue
            if TABLES.search(line) or NAMED.search(line):
                found.append(f"{rel}:{n}: {line.strip()}")
    return found


def main() -> int:
    found = offenders()
    if found:
        print("physmap: a physical address is dereferenced without the physmap conversion:")
        for line in found:
            print(f"  {line}")
        print(
            "  Fix: go through phys_to_virt / frame_ptr / table_ptr."
            " If the identity really is the right view (host fixture, boot path, the identity arm"
            " of a pointer function), say so on the line with `physmap-ok: <reason>`."
        )
        return 1
    print("physmap: no physical address is used as a pointer (PHYSMAP.md P6)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
