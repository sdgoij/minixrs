#!/usr/bin/env python3
"""The minix userland C targets, and what a C build has to be told to name one.

Three drivers need the same answers — `tools/cc-minix.py` (the `cc` a C project's
own build calls), `tools/build-c-hello.py` (the smoke binaries) and
`tools/build-bash.py` — so the table lives here rather than three times over.
What the answers are is the point: the *minix* triple is what the rustc rlib is
built for, and the clang *machine* is what the headers and the object code are
compiled for. They are the same machine, one per arch, and the link step is
where the two are made to agree.
"""

from __future__ import annotations

import dataclasses
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]


@dataclasses.dataclass(frozen=True)
class Arch:
    """One minix userland target."""

    name: str
    """The short name the Justfile and the recipes use (`x86`, `riscv64`, ...)."""

    triple: str
    """The minix triple: what the rlib is built for and what rustc links for."""

    machine: str
    """The machine clang is told (`--target=<machine>-unknown-none`), which is
    also the suffix of the arch's crt0."""

    red_zone: bool
    """Whether `-mno-red-zone` means anything here. It is an x86_64 and AArch64
    flag; RISC-V has no red zone, and clang only warns that it is unused."""

    e_machine: int
    """`e_machine` as the ELF header carries it (`EM_X86_64`, `EM_AARCH64`,
    `EM_RISCV`), so a built artifact can be checked against the target it was
    asked for rather than trusted to be the right one."""

    extra_cflags: tuple[str, ...] = ()
    """Flags this machine needs beyond the common set, where clang's own default
    disagrees with what the minix target's rustc emits.

    RISC-V is the case: `riscv64-unknown-none` defaults to the *soft-float* ABI,
    while the fork's `riscv64gc-unknown-minix` spec is `Lp64d`
    (`riscv64gc_unknown_minix.rs`), and lld refuses to link objects whose
    `EF_RISCV_FLOAT_ABI` differ — which is also a correctness question, not just a
    link error: `printf("%f")`'s double would cross the boundary in a register
    one side does not use."""

    @property
    def clang_target(self) -> str:
        return f"{self.machine}-unknown-none"

    @property
    def crt0(self) -> pathlib.Path:
        return ROOT / "tools" / f"crt0-{self.machine}.S"

    @property
    def libc_deps(self) -> pathlib.Path:
        return ROOT / "target" / self.triple / "release" / "deps"

    def base_cflags(self) -> list[str]:
        """The flags every freestanding compile of C for this target needs.

        `-fno-pic` matches the kernel's static relocation model, and the
        red-zone flag matches the fork spec's `disable_redzone`. What keeps the
        compile off the host's headers is `tools/ccflags.py`, not this list.
        """
        flags = [f"--target={self.clang_target}", "-ffreestanding"]
        if self.red_zone:
            flags.append("-mno-red-zone")
        flags += list(self.extra_cflags)
        flags += ["-fno-stack-protector", "-fno-pic"]
        return flags


X86_64 = Arch("x86", "x86_64-pc-minix", "x86_64", red_zone=True, e_machine=0x3E)
RISCV64 = Arch("riscv64", "riscv64gc-unknown-minix", "riscv64", red_zone=False,
               e_machine=0xF3, extra_cflags=("-march=rv64gc", "-mabi=lp64d"))
AARCH64 = Arch("aarch64", "aarch64-unknown-minix", "aarch64", red_zone=True, e_machine=0xB7)

ALL = (X86_64, RISCV64, AARCH64)

DEFAULT = X86_64
"""What a build that names no target gets: x86_64, which is what every caller
did before there was a choice."""

# Both the short names and the triples resolve, the way
# `tools/build-std-hello.py` accepts them; `all` is what the Justfile passes to
# mean "every target".
BY_NAME = {arch.name: arch for arch in ALL} | {arch.triple: arch for arch in ALL}


def resolve(token: str) -> Arch:
    """The arch *token* names, or a die naming the ones that exist."""
    if token in BY_NAME:
        return BY_NAME[token]
    known = ", ".join(arch.name for arch in ALL)
    sys.exit(f"error: unknown target {token!r} (expected one of {known}, or a "
             f"minix triple)")


def resolve_argv(argv: list[str]) -> tuple[Arch, list[str]]:
    """Split an optional leading target off a command line.

    The target is a bare word, so it cannot be confused with a compiler flag or
    an input file: with none, the rest of the line is the whole line and the
    target is the default.
    """
    if argv and argv[0] in BY_NAME:
        return BY_NAME[argv[0]], argv[1:]
    return DEFAULT, argv
