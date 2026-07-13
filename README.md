# minixrs

> **⚠️ Research project — not production-ready.**  
> If you're looking for a production operating system, use Linux, a BSD, or [Redox](https://www.redox-os.org/) instead.

A Rust port of [MINIX 3.3.0](https://www.minix3.org/), written from scratch.

This project implements the full MINIX 3 stack in Rust — kernel, architecture-specific code, device drivers, filesystem servers, networking, system servers, and userland programs — targeting both **x86_64** and **RISC-V64**.

## Status

Boots multi-process userspace in QEMU on x86_64 with a serial shell.
VFS mounts the root filesystem and MFS can read directories, but
grant-based data transfer (`virtual_copy`) and external binary exec
are still works in progress.

See `.agents/skills/` for domain-specific documentation and
[PORTING_PLAN.md](PORTING_PLAN.md) for the task tracker.

## Quick Start

### Prerequisites

- Rust toolchain (MSRV: **1.96**, edition: **2024**)
- QEMU (`qemu-system-x86_64`, `qemu-system-riscv64`)
- `rust-objcopy`, `rust-nm`, `rust-lld` (from `rust-src` component)
- Clang (for trampoline bootstrap)
- [Just](https://just.systems/) (build runner)

### One-Time Bootstrap

jsh is a custom shell that handles cross-platform path resolution.
Compile it once before using `just`:

```bash
# Windows (manual):
rustc tools/jsh.rs -o target/jsh

# Linux (shebang, or just run the same command):
just prepare
```

### Usage

```bash
# x86_64
just build                    # Build the kernel
just run                      # Build and boot in QEMU
just debug                    # Build and boot with GDB server on :1234

# RISC-V64 (requires nightly)
just build riscv64            # Build the RISC-V kernel
just run riscv64              # Boot in QEMU (uses OpenSBI)
just test-qemu riscv64        # Run integration tests
```

## Project Structure

```
crates/
├── kernel              # Core kernel: processes, scheduling, IPC, VM
├── kernel-boot         # Boot loader & entry point (x86_64 trampoline)
├── arch-common         # Architecture-independent kernel types & ABI
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

The project supports both **x86_64** and **RISC-V64** targets via architecture-specific crates (`arch-x86_64`, `arch-riscv64`) sharing a common core (`arch-common`).

See `.agents/skills/` for domain deep-dives:
- `minix-boot-process` — boot chain from QEMU to shell
- `minix-ipc-patterns` — message formats, SENDREC semantics, grants
- `minix-server-patterns` — main loop, dispatch, SEF callbacks
- `minix-c-to-rust` — struct layout, type mapping, no-stubs policy

## Testing

- **Host tests:** `cargo test` — pure-logic unit and property tests
- **QEMU integration:** `just test-qemu` — 40 kernel tests running in-ring 0
  (page tables, IPC, scheduler, timers, syscalls, ELF loading, grants)
- **Boot tests:** `just test-boot` — multi-server verification after VFS mount_root
  (server liveness, IPC round-trips, filesystem metadata)

See `minix-testing` skill in `.agents/skills/` for full patterns and isolation mechanisms.

The original C reference source is at `.refs/minix-3.3.0/` (git submodule).

## Build & Development

- **Build runner:** `Justfile` — run `just <recipe>` for available commands
- **Cargo features:**
  - `embed_initramfs` — embed initramfs in the kernel binary
  - `embed_minixfs` — embed minixfs driver in the kernel
  - `qemu-tests` — enable QEMU integration test infrastructure
- **RISC-V64** requires the nightly toolchain (`-Zbuild-std`)

## License

Licensed under the [GNU General Public License v2.0](LICENSE.md).

MINIX 3 source code references are used under the [LICENSE.MINIX](LICENSE.MINIX).
