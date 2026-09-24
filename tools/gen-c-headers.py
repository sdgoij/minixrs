#!/usr/bin/env python3
"""Generate the C headers from minix-libc.

The libc is the source of truth: cbindgen reads its `#[no_mangle] extern "C"`
surface and its `#[repr(C)]` layouts. cbindgen selects whole crates, not
individual C headers, so it emits once and this script buckets the items: a
function or object goes to the C header its module belongs to, and every type
lands in the shared `minix-types.h` that the others include.

Output goes to `target/c-include/` -- generated, never hand-edited -- so the C
headers cannot drift from the implementation.

Requires cbindgen (Fedora: `dnf install cbindgen`).

    python3 tools/gen-c-headers.py
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
LIBC = ROOT / "crates" / "minix-libc"
SRC = LIBC / "src"
OUT = ROOT / "target" / "c-include"
TYPES_HEADER = "minix-types.h"

# Attributes are not always adjacent to the item: `#[unsafe(naked)]` sits
# between no_mangle and `pub` in c_setjmp.rs, and lib.rs puts the `# Safety`
# doc comment between them. So the run of attributes *and* comments is
# captured, then checked for `no_mangle`.
ATTR = r"(?:#\[[^\]]*\]\s*|//[^\n]*\n\s*|/\*(?:[^*]|\*(?!/))*\*/\s*)"
ITEM = re.compile(
    "(" + ATTR + "*)"
    r"pub\s+(?:unsafe\s+)?"
    r"(?:extern\s+\"C\"\s+fn|static\s+mut)\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)"
)

# Which C header a module's exports belong to. A module whose exports span
# several headers (c_sys.rs covers unistd/dirent/stat/pwd/socket) belongs to
# the first here; splitting it needs a per-symbol table, which is the next
# refinement rather than a guess.
MODULE_HEADER = {
    "lib.rs": "unistd.h",
    "c_locale.rs": "locale.h",
    "c_net.rs": "netinet/in.h",
    "c_setjmp.rs": "setjmp.h",
    "c_stdio.rs": "stdio.h",
    "c_stdlib.rs": "stdlib.h",
    "c_string.rs": "string.h",
    "c_sys.rs": "unistd.h",
    "c_termios.rs": "termios.h",
    "c_time.rs": "time.h",
    "c_wchar.rs": "wchar.h",
    "pthread.rs": "pthread.h",
}

# Symbols whose C header is not their module's: POSIX files these elsewhere
# than where this crate's modules put them.
SYMBOL_HEADER = {
    "system": "stdlib.h",
    "__assert_fail": "assert.h",
    "rename": "stdio.h",
    # ioctl is declared in sys/ioctl.h, not unistd.h, though c_sys.rs owns it.
    "ioctl": "sys/ioctl.h",
    # mknod/mkfifo belong to sys/stat.h for the same reason.
    "mknod": "sys/stat.h",
    "mkfifo": "sys/stat.h",
    # ... and the address conversions to arpa/inet.h, though c_net.rs owns them.
    "inet_addr": "arpa/inet.h",
    "inet_aton": "arpa/inet.h",
    "inet_ntoa": "arpa/inet.h",
}


def owners() -> dict[str, str]:
    """Item name -> the C header it is declared in, from the libc's exports."""
    owner: dict[str, str] = {}
    for path in sorted(SRC.glob("*.rs")):
        header = MODULE_HEADER.get(path.name)
        if header is None:
            continue
        for attrs, name in ITEM.findall(path.read_text(encoding="utf-8")):
            if "no_mangle" in attrs:
                owner[name] = SYMBOL_HEADER.get(name, header)
    return owner


def cbindgen_text() -> str:
    """The whole crate's C surface, as cbindgen renders it."""
    result = subprocess.run(
        ["cbindgen", "--lang", "c", "--output", "/dev/stdout", str(LIBC)],
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout


def items(text: str) -> tuple[list[str], list[str]]:
    """Split the output into (leading #include lines, one chunk per item).

    An item ends at a `;` outside braces and outside any comment, which is
    what a declaration, a typedef or a struct definition all satisfy.
    """
    preamble: list[str] = []
    chunks: list[str] = []
    buf: list[str] = []
    depth = 0
    in_comment = False

    for line in text.splitlines(keepends=True):
        stripped = line.strip()
        if not buf:
            if not stripped or stripped.startswith("#ifndef") or stripped.startswith("#define"):
                continue
            if stripped.startswith("#include"):
                preamble.append(line)
                continue
            if stripped.startswith("/*") and not stripped.endswith("*/"):
                in_comment = True
        buf.append(line)
        if in_comment:
            if "*/" in line:
                in_comment = False
            continue
        depth += line.count("{") - line.count("}")
        if depth == 0 and stripped.endswith(";"):
            chunks.append("".join(buf))
            buf = []
    if buf:
        chunks.append("".join(buf))
    return preamble, chunks


def item_name(block: str) -> str | None:
    # Doc comments travel with the item and mention other names (`waitpid()`
    # in prose), so they must not take part in naming it.
    code = re.sub(r"/\*.*?\*/", "", block, flags=re.S)
    match = re.search(r"typedef\s+struct\s+(\w+)\s*\{", code)
    if match:
        return match.group(1)
    match = re.search(r"^typedef\s+.*\b(\w+)\s*;", code, re.S | re.M)
    if match:
        return match.group(1)
    match = re.search(r"\b(\w+)\s*\(", code)
    if match:
        return match.group(1)
    match = re.search(r"\b(\w+)\s*;", code)
    return match.group(1) if match else None


def guard(name: str) -> str:
    return "MINIX_GENERATED_" + name.upper().replace(".", "_").replace("/", "_")


def write_header(path: pathlib.Path, body: str, includes: list[str], preamble: list[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [f"#ifndef {guard(path.name)}", f"#define {guard(path.name)}", ""]
    lines.append("".join(preamble).rstrip())
    if lines[-1].strip() and not lines[-1].startswith("#include"):
        lines.append("")
    lines += includes
    lines += ["", body.rstrip(), "", f"#endif /* {guard(path.name)} */", ""]
    path.write_text("\n".join(lines), encoding="utf-8")


def main() -> int:
    owner = owners()
    preamble, chunks = items(cbindgen_text())
    if not chunks:
        print("cbindgen produced no items", file=sys.stderr)
        return 1

    per_header: dict[str, list[str]] = {}
    types: list[str] = []
    unassigned: list[str] = []
    for chunk in chunks:
        name = item_name(chunk)
        header = owner.get(name) if name else None
        if header is None:
            types.append(chunk)
            if name and name[0].islower():
                unassigned.append(name)
        else:
            per_header.setdefault(header, []).append(chunk)

    if unassigned:
        print("exports with no header (falling back to types): " + ", ".join(sorted(unassigned)))

    write_header(OUT / TYPES_HEADER, "\n".join(types), [], preamble)
    print(f"  {TYPES_HEADER:16} {len(types):3} items")
    for header, body in sorted(per_header.items()):
        write_header(OUT / header, "\n".join(body), [f'#include "{TYPES_HEADER}"'], preamble)
        print(f"  {header:16} {len(body):3} items")
    print(f"generated {1 + len(per_header)} headers into {OUT.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
