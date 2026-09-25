#!/usr/bin/env python3
"""Measure how much of a linked minix Rust binary a shared layer could share.

Phase 4 of `DYNAMIC_LINKING.md` asks whether a dynamic `std`/`minix-std` would pay for
itself. The answer turns on two numbers per binary, and this reports both:

* how much of a binary is **monomorphised instantiations** (`_RI...`): a generic or
  `#[inline]` body compiled into this binary. A shared object cannot share those — every
  binary keeps its own copy whatever the linking scheme.
* of the rest, how much belongs to the **layer** (`minix_std`/`minix_rt`/`minix_util`/
  `net`/`core`/`alloc`/`compiler_builtins`) rather than to the program that was linked.
  Only this part is even a candidate for sharing.

Sizes come from `llvm-nm --print-size`, which counts a symbol's own bytes: code, data and
bss, so a layer's bss is in the "layer" column even though a shared object would not share
it. Treat the numbers as an upper bound on the win.

Rust's v0 mangling names the defining crate (`Cs<hash>_<len><name>`), which is what makes
the attribution possible at all. Instantiations are attributed to the crate that *defines*
the generic, not the one that instantiates it, because that is the honest reading of "whose
code is this".

Usage: python tools/rc-cost.py <binary>...

Note the binaries in `target/<triple>/release/` keep their symbols (the userland build does
not strip); one built with `-C strip=symbols`, like `coreutils`, reports nothing.
"""

import collections
import pathlib
import re
import subprocess
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from lld import find_nm  # noqa: E402
# Rust v0 mangling: `Cs` + a base-62 disambiguator + `_` + the crate name's decimal
# length + the name. The disambiguator's length varies with its value, which is why
# this is `+` and not a fixed width.
CRATE = re.compile(r"Cs[0-9A-Za-z]+_(\d+)([0-9A-Za-z_]+)")

LAYER = {
    "minix_std",
    "minix_rt",
    "minix_util",
    "net",
    "core",
    "alloc",
    "compiler_builtins",
    "hashbrown",
    "memchr",
    "getrandom",
}


def analyse(path: pathlib.Path) -> None:
    nm = find_nm()
    if nm is None:
        sys.exit("error: no llvm-nm to read the symbols with (set MINIXRS_LLVM_NM)")
    out = subprocess.run(
        [str(nm), "--print-size", str(path)], capture_output=True, text=True
    ).stdout
    instantiated = 0
    layer = 0
    program = 0
    buckets: collections.Counter = collections.Counter()
    for line in out.splitlines():
        f = line.split()
        # `--print-size`: address, size, type, name. An undefined symbol has neither
        # address nor size, so it has fewer fields and is skipped.
        if len(f) < 4:
            continue
        try:
            size = int(f[-3], 16)
        except ValueError:
            continue
        name = f[-1]
        if not name.startswith("_R"):
            program += size
            buckets["(c / asm)"] += size
            continue
        if name.startswith("_RI"):
            instantiated += size
            continue
        m = CRATE.search(name)
        crate = m.group(2)[: int(m.group(1))] if m else "?"
        buckets[crate] += size
        if crate in LAYER:
            layer += size
        else:
            program += size

    total = instantiated + layer + program
    pct = lambda n: n * 100 // max(total, 1)  # noqa: E731
    print(f"\n{path.name}: {total} bytes of symbols")
    print(f"  {instantiated:8d}  {pct(instantiated):3d}%  instantiated generics")
    print(f"  {layer:8d}  {pct(layer):3d}%  layer functions")
    print(f"  {program:8d}  {pct(program):3d}%  the program")
    print("  top: " + ", ".join(f"{k} {v}" for k, v in buckets.most_common(5)))


def main(argv: list[str]) -> int:
    if not argv:
        sys.exit(__doc__)
    for a in argv:
        analyse(pathlib.Path(a))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
