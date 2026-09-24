#!/usr/bin/env python3
"""Cross-check tools/c-include against the C surface minix-libc exports.

The libc is the source of truth for these headers, so the two sides have to
agree in both directions:

* an export with no declaration gives a C caller the implicit-declaration
  path, which silently assumes `int` -- how `fdopen` went missing from
  `stdio.h` and turned five call sites into "assigning to FILE * from int";
* a declaration with no export is a promise the link cannot keep.

    python3 tools/check-c-headers.py [--verbose]

Exit status is 1 when either side has an unmatched entry.
"""

from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
LIBC = ROOT / "crates" / "minix-libc" / "src"
INC = ROOT / "tools" / "c-include"

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

# A declaration is a prototype line: not a macro, not a typedef, and it names a
# function. Only the leading identifier is taken, which is the function name.
DECL = re.compile(r"^[A-Za-z_][A-Za-z0-9_ \t*]*?\b([A-Za-z_][A-Za-z0-9_]*)\s*\(")
# An object declaration (`extern FILE *stdin;`) names the object after its type.
DECL_OBJECT = re.compile(r"^extern\b[^;=()]*?\b([A-Za-z_][A-Za-z0-9_]*)\s*;")


def exports() -> dict[str, str]:
    out: dict[str, str] = {}
    for path in sorted(LIBC.glob("*.rs")):
        text = path.read_text(encoding="utf-8")
        for attrs, name in ITEM.findall(text):
            if "no_mangle" in attrs:
                out[name] = path.name
    return out


def declarations() -> dict[str, str]:
    """Every name a C caller can see, function or object, per header.

    A prototype wraps over several lines, and a line-at-a-time matcher misses
    those: `pthread_create`, `qsort`, `sendto` and a dozen more were reported as
    undeclared while sitting in the headers. So a declaration is accumulated
    until its `;` and only then matched.
    """
    out: dict[str, str] = {}
    for path in sorted(INC.rglob("*.h")):
        rel = path.relative_to(INC).as_posix()
        depth = 0
        pending = ""
        for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
            line = raw.strip()
            if depth > 0:
                depth += line.count("{") - line.count("}")
                continue
            if not line or line.startswith(("#", "//", "/*", "*", "typedef", 'extern "C"')):
                continue
            if "{" in line:
                # A body: a struct, a union, an enum, an inline function. Its
                # members and locals are not names a declaration puts in scope,
                # and they end in `;` like a prototype does.
                depth = line.count("{") - line.count("}")
                pending = ""
                continue
            if not pending and not (line[0].isalpha() or line[0] == "_"):
                # Not where a declaration starts: a macro's continuation, a
                # stray brace, the tail of a comment.
                continue
            pending = f"{pending} {line}".strip()
            if not pending.endswith(";"):
                if len(pending) > 400:
                    pending = ""
                continue
            for pattern in (DECL, DECL_OBJECT):
                match = pattern.match(pending)
                if match:
                    out.setdefault(match.group(1), rel)
                    break
            pending = ""
    return out


def main() -> int:
    verbose = "--verbose" in sys.argv
    exp, dec = exports(), declarations()

    # The port's own C-ABI internals and the libc++ hooks: no C header declares
    # them, and no C library is expected to. Everything else the libc exports is
    # meant to be callable from C, and an export with no declaration gives the
    # caller the implicit `int` return type.
    internals = {
        "__cxa_atexit",
        "__cxa_finalize",
        "__minix_init_array",
        "__minix_set_environ",
        "minix_libc_tls_init",
    }
    undeclared = sorted(n for n in exp if n not in dec and n not in internals)
    unimplemented = sorted(n for n in dec if n not in exp)

    print(f"exports: {len(exp)}   declarations: {len(dec)}")
    print(f"\nexported but not declared anywhere ({len(undeclared)}):")
    for name in undeclared:
        print(f"  {name:22} ({exp[name]})")

    print(f"\ndeclared but not exported ({len(unimplemented)}):")
    for name in unimplemented:
        print(f"  {name:22} ({dec[name]})")

    if verbose:
        print("\ndeclarations per header:")
        per: dict[str, int] = {}
        for header in dec.values():
            per[header] = per.get(header, 0) + 1
        for header, count in sorted(per.items()):
            print(f"  {count:4}  {header}")

    return 1 if undeclared or unimplemented else 0


if __name__ == "__main__":
    sys.exit(main())
