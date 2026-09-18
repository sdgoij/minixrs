#!/usr/bin/env python3
"""Verify a published stage1 by consuming it the way a Linux dev or CI job does.

Run it on Linux, or in WSL against a Windows checkout:

    wsl.exe -d <distro> -- python3 tools/verify-stage1.py

The asset is host-specific, so on Windows or macOS it only reports that nothing
is published for that host.

Nothing here re-implements the release. `tools/fetch-stage1.py` installs the
toolchain for the commit the `rust` submodule pins - checksum, extraction and its
own assertions - and this then links `tools/std-hello.rs` for all three minix
targets with the linker the Justfile resolves (the stage1 sysroot's own
`rust-lld`, see `tools/lld.py`) and reads the ELF header of each result. A host
that reaches the summary can run the published toolchain and link with it, which
is the part a Windows build machine cannot check.

Environment:
  MINIXRS_STAGE1_BASE   fetch from another base (a URL, or `file://` a directory)
                        instead of the release on GitHub
  MINIXRS_STAGE1_DEST   install directory (default: `target/stage1-verify`); an
                        install for the pinned commit is reused, so a second run
                        does not download 230 MB again
"""

from __future__ import annotations

import os
import pathlib
import struct
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
FETCH = ROOT / "tools" / "fetch-stage1.py"
HELLO = ROOT / "tools" / "std-hello.rs"
LINKER_SCRIPT = ROOT / "tools" / "minix-user.ld"
DEFAULT_DEST = ROOT / "target" / "stage1-verify"
MARKER = ".minixrs-fetched"

# rustc triple -> the e_machine its ELF has to carry.
TARGETS = {
    "x86_64-pc-minix": 0x3E,
    "riscv64gc-unknown-minix": 0xF3,
    "aarch64-unknown-minix": 0xB7,
}
MACHINES = {0x3E: "x86-64", 0xF3: "RISC-V", 0xB7: "AArch64"}


def pinned_sha() -> str:
    """The commit the `rust` submodule pins, which names the release tag."""
    result = subprocess.run(
        ["git", "-C", str(ROOT / "rust"), "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        sys.exit(f"error: cannot read the rust submodule commit: {result.stderr.strip()}")
    return result.stdout.strip()


def install(dest: pathlib.Path, sha: str) -> pathlib.Path:
    """Install the published toolchain through the consumer script."""
    stage1 = dest / "stage1"
    marker = stage1 / MARKER
    recorded = marker.read_text(encoding="utf-8") if marker.is_file() else ""

    force = []
    if not recorded.startswith(sha):
        force = ["--force"]
        if stage1.is_dir():
            print(f"== replacing {stage1}: it is not the published toolchain for {sha[:12]}")

    print(f"== installing the stage1 for {sha[:12]} with tools/fetch-stage1.py")
    env = {**os.environ, "MINIXRS_STAGE1_DEST": str(dest)}
    if subprocess.run([sys.executable, str(FETCH), *force], env=env).returncode != 0:
        sys.exit(f"error: {FETCH.name} failed - nothing to verify")

    if not (stage1 / "bin").is_dir():
        sys.exit(f"error: {FETCH.name} installed no toolchain at {stage1}")
    return stage1


def sysroot_lld(stage1: pathlib.Path) -> pathlib.Path:
    """The linker the Justfile resolves for rustc: the sysroot's own LLD.

    `gcc-ld` is installed next to it for the host only, which is what tells the
    host's copy apart from a target's.
    """
    for lld in sorted(stage1.glob("lib/rustlib/*/bin/rust-lld")):
        if (lld.parent / "gcc-ld").is_dir():
            return lld
    sys.exit(
        f"error: no rust-lld under {stage1}/lib/rustlib/*/bin - the toolchain cannot "
        "link minix binaries, which is one of the things this checks for"
    )


def elf_machine(path: pathlib.Path) -> int:
    header = path.read_bytes()[:20]
    if header[:4] != b"\x7fELF":
        sys.exit(f"error: {path} is not an ELF file")
    order = "<" if header[5] == 1 else ">"
    return struct.unpack_from(f"{order}H", header, 18)[0]


def main() -> int:
    # Line-buffered so the banner stays above the child processes' output when
    # stdout is a pipe (CI, or `| tail`).
    sys.stdout.reconfigure(line_buffering=True)

    dest = pathlib.Path(os.environ.get("MINIXRS_STAGE1_DEST") or DEFAULT_DEST)
    sha = pinned_sha()
    stage1 = install(dest, sha)

    rustc = stage1 / "bin" / "rustc"
    lld = sysroot_lld(stage1)
    print(f"== toolchain: {rustc}")
    subprocess.run([str(rustc), "-vV"], check=True)
    subprocess.run([str(lld), "-flavor", "gnu", "--version"], check=True)

    print(f"== linking tools/std-hello.rs for {len(TARGETS)} minix targets")
    for target, machine in TARGETS.items():
        out = dest / f"hello-{target}"
        subprocess.run(
            [
                str(rustc),
                "--target", target,
                "--edition", "2024",
                "-C", f"link-arg=-T{LINKER_SCRIPT}",
                "-C", "link-arg=--no-eh-frame-hdr",
                "-C", f"linker={lld}",
                "-o", str(out),
                str(HELLO),
            ],
            check=True,
        )
        got = elf_machine(out)
        if got != machine:
            sys.exit(
                f"error: {out} is {MACHINES.get(got, hex(got))}, expected {MACHINES[machine]}"
            )
        print(f"   {target:26} {MACHINES[machine]:8} {out.stat().st_size} bytes")

    print("ok: the published stage1 runs and links all three minix targets on this host")
    return 0


if __name__ == "__main__":
    sys.exit(main())
