# minixrs

> **⚠️ Research project — not production-ready.**  
> If you're looking for a production operating system, use Linux, a BSD, or [Redox](https://www.redox-os.org/) instead.

A Rust port of [MINIX 3.3.0](https://www.minix3.org/), written from scratch.

This project implements the full MINIX 3 stack in Rust — kernel, architecture-specific code, device drivers, filesystem servers, networking, system servers, and userland programs — targeting **x86_64**, **RISC-V64**, and **AArch64**.

## Status

Boots multi-process userspace in QEMU on x86_64, RISC-V64, and AArch64 with a serial shell.
VFS mounts the root filesystem, MFS reads and writes files, and the shell supports
`>` redirection (create/truncate) for builtin commands **and external binaries**
(exec'd commands write through VFS via a dup2'd fd, so `/bin/echo x > file` works).
Pipes are not wired into the shell parser yet.

See `.agents/skills/` for domain-specific documentation and
[PORTING_PLAN.md](PORTING_PLAN.md) for the task tracker.

## Quick Start

### Prerequisites

- Rust toolchain (MSRV: **1.96**, edition: **2024**) + **nightly** (`-Zbuild-std`)
- bash on PATH (git-bash on Windows) — the Justfile recipes are POSIX sh
- QEMU (`qemu-system-x86_64`, `qemu-system-riscv64`, `qemu-system-aarch64`)
- `rust-objcopy`, `rust-nm`, `rust-lld` (from `rust-src` component)
- Clang (for the x86 trampoline post-link)
- [Just](https://just.systems/) (build runner)

> **Windows users:** Just executes recipes with `sh` (the POSIX shell) —
> without it, Just falls back to `cmd` and the recipes break. Git for
> Windows ships one at `C:\Program Files\Git\usr\bin\sh.exe`; add that
> directory to your `PATH` (or `C:\Program Files\Git\bin`). See
> <https://github.com/casey/just#windows> for how Just selects its shell.

### Usage

```bash
# x86_64
just build                    # Build the kernel + boot images
just run                      # Build and boot in QEMU
just debug                    # Build and boot with GDB server on :1234
just test-qemu                # Run the QEMU integration tests

# RISC-V64 (requires nightly)
just build riscv64            # Build the RISC-V kernel
just run riscv64              # Boot in QEMU (uses OpenSBI)
just test-qemu riscv64        # Run the QEMU integration tests
just test-boot riscv64        # Run the boot tests

# AArch64 (requires nightly)
just build aarch64            # Build the AArch64 kernel
just run aarch64              # Boot in QEMU (virt machine)
just debug aarch64            # Build and boot with GDB server on :1234
just test-qemu aarch64        # Run the QEMU integration tests
just test-boot aarch64        # Run the boot tests
```

The Just recipes orchestrate plain `cargo` invocations; the initramfs CPIO
and MinixFS root image are assembled by `crates/kernel/build.rs` from the
built userland/server binaries, and the x86 trampoline/kernel.bin post-link
is handled by `tools/mkboot.rs`. Assembled images are mirrored per-target
under `target/images/<triple>/` for host inspection.

## Project Structure

```
crates/
├── kernel              # Core kernel: processes, scheduling, IPC, VM
├── kernel-boot         # Boot loader & entry point (x86_64 trampoline)
├── boot-image          # Initramfs CPIO + MinixFS image builders (host)
├── arch-common         # Architecture-independent kernel types & ABI
├── arch-aarch64        # AArch64-specific kernel code
├── arch-x86_64         # x86_64-specific kernel code
├── arch-riscv64        # RISC-V64-specific kernel code
├── drivers             # Device drivers (serial, keyboard, etc.)
├── fs                  # Filesystem servers (minixfs, ramdisk, etc.)
├── net                 # Networking stack
├── servers             # System servers (PM, VFS, VM, sched, TTY, etc.)
├── userland            # Userland binaries (cat, ls, sh, etc.)
├── minix-rt            # Userspace runtime: _start, panic handler, syscalls
├── minix-std           # MINIX syscall layer: IPC, endpoints, grants
├── minix-libc          # Minimal libc for FFI
├── libs                # libc, libm, libutil re-implementation
└── minix-util          # Shared utilities
```

## Architecture

MINIX 3's microkernel design is preserved:

- **Kernel** — process table, scheduling, IPC, virtual memory
- **System servers** — separate user-space processes (PM, VFS, VM, sched, TTY, DS, RS, MFS, ramdisk)
- **Drivers** — hardware abstraction, registered with the kernel
- **Filesystem servers** — minixfs, ramdisk, etc.
- **VFS** — virtual filesystem layer for unified file operations
- **Userland** — classic POSIX utilities (cat, ls, cp, rm, sh, etc.)

The project supports **x86_64**, **RISC-V64**, and **AArch64** targets via architecture-specific crates (`arch-x86_64`, `arch-riscv64`, `arch-aarch64`) sharing a common core (`arch-common`).

See `.agents/skills/` for domain deep-dives:
- `minix-boot-process` — boot chain from QEMU to shell
- `minix-ipc-patterns` — message formats, SENDREC semantics, grants
- `minix-server-patterns` — main loop, dispatch, SEF callbacks
- `minix-c-to-rust` — struct layout, type mapping, no-stubs policy

## Testing

- **Host tests:** `cargo test` — pure-logic unit and property tests
- **QEMU integration:** `just test-qemu [arch]` — kernel tests running in QEMU
  (page tables, IPC, scheduler, timers, syscalls, ELF loading, grants):
  - `just test-qemu` (x86_64) — 82 tests, exits with a real pass/fail code
  - `just test-qemu riscv64` — 25 tests (4 arch-specific skips)
  - `just test-qemu aarch64` — 23 tests (4 arch-specific skips)
- **Boot tests:** `just test-boot [arch]` — multi-server verification after VFS
  mount_root on all three arches (server liveness, process-table consistency,
  brk/RAM-disk mappings, IPC, filesystem metadata, allocator)

RISC-V and AArch64 exit QEMU via SBI reset / PSCI (no exit-code device on
this QEMU build), so their recipes pass/fail on the serial log markers.

See `minix-testing` skill in `.agents/skills/` for full patterns and isolation mechanisms.

The original C reference source is at `.refs/minix-3.3.0/` (git submodule).

## Build & Development

- **Build runner:** `Justfile` — run `just <recipe>` for available commands
- **Cargo features:**
  - `embed_initramfs` — embed initramfs in the kernel binary
  - `embed_minixfs` — embed minixfs driver in the kernel
  - `qemu-tests` — enable QEMU integration test infrastructure
- **RISC-V64 and AArch64** require the nightly toolchain (`-Zbuild-std`)

## License

Licensed under the [GNU General Public License v2.0](LICENSE.md).

MINIX 3 source code references are used under the [LICENSE.MINIX](LICENSE.MINIX).
