#!/usr/bin/env python3
"""The flags that make a C compile for minix hermetic.

Without them, `clang --target=x86_64-unknown-none -I tools/c-include` also
searches the host's `/usr/include`, so **any header the port does not provide is
silently satisfied by glibc** and any question asked about it is answered by the
host: configure reported `HAVE_TERMIOS_H`, `HAVE_SYS_WAIT_H` and
`HAVE_UNION_WAIT` for a target that has none of them, and bash then compiled
paths no real system has. A missing header should be an error, not a guess.

Two call sites share this list — `tools/build-c-hello.py` and the scratch
`target/tmp/cc-minix` the bash build uses — so it lives here rather than in
both. `C_BUILD.md` has the write-up.
"""

from __future__ import annotations

import pathlib
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
INCLUDE = ROOT / "tools" / "c-include"


def resource_dir(clang: str = "clang") -> str:
    """clang's own include dir, which holds the headers a freestanding compile
    is entitled to (`stddef.h`, `stdint.h`, `stdarg.h`, `limits.h`, ...).

    Asked of the compiler rather than hardcoded: the path names its version and
    differs between the Windows and Linux installs, which are two different
    clang builds.
    """
    out = subprocess.run(
        [clang, "-print-resource-dir"], capture_output=True, text=True, check=True
    )
    return out.stdout.strip()


def compile_flags(clang: str = "clang") -> list[str]:
    """`-nostdinc`, the compiler's own headers, and the port's.

    Order matters: `-I` is searched before `-isystem`, so the port's `stddef.h`,
    `stdint.h` and the rest win over clang's where both define them, and clang's
    are the fallback for what the port has no opinion about.
    """
    return ["-nostdinc", "-isystem", f"{resource_dir(clang)}/include", f"-I{INCLUDE}"]


# The arguments in a `cc` line that the *link* step needs, as opposed to the
# compile step. A wrapper that forwards `-l`/`-L` to nothing links an executable
# with no archives in it, which shows up as undefined symbols for whole
# libraries: bash's link line carries `-lbuiltins -lglob -lsh -lreadline ...`.
LINK_FLAGS = ("-L", "-l", "-Wl,", "-static", "-rdynamic", "-pthread")
LINK_FLAGS_WITH_VALUE = ("-Xlinker", "-u", "-z")


def link_passthrough(argv: list[str]) -> list[str]:
    """The link-step arguments of a `cc` command line, in order."""
    out: list[str] = []
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg in LINK_FLAGS_WITH_VALUE:
            if i + 1 < len(argv):
                out += [arg, argv[i + 1]]
                i += 2
                continue
        elif any(arg.startswith(flag) for flag in LINK_FLAGS):
            out.append(arg)
        i += 1
    return out
