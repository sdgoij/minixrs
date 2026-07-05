# minixrs

A Rust port of [MINIX 3.3.0](https://www.minix3.org/), written from scratch.

This project implements the full MINIX 3 stack in Rust — kernel, architecture-specific code, device drivers, filesystem servers, networking, system servers, and userland programs — targeting both **x86_64** and **RISC-V64**.

## Status

[See [PORTING_PLAN.md](PORTING_PLAN.md) for the phased implementation roadmap and [TESTING.md](TESTING.md) for test coverage details.]

## Quick Start

### Prerequisites

- Rust toolchain (MSRV: **1.96**, edition: **2024**)
- QEMU (`qemu-system-x86_64`, `qemu-system-riscv64`)
- `rust-objcopy`, `rust-nm`, `rust-lld` (from `rust-src` component)
- Clang (for trampoline bootstrap)
- [Just](https://just.systems/) (build runner)

### x86_64

```bash
just build          # Build the kernel
just run            # Build and boot in QEMU
just test-qemu      # Run QEMU integration tests
just image          # Build a bootable disk image (minix.img)
just run-img        # Boot the disk image
```

### RISC-V64

```bash
just build-riscv64   # Build the RISC-V kernel (requires nightly toolchain)
just run-riscv64     # Boot in QEMU (uses OpenSBI)
just test-qemu-riscv # Run integration tests
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

## Testing

- **~600+ tests** across all crates (`cargo test`)
- **QEMU integration tests** that boot the kernel, run assertions, and verify behavior end-to-end
- See [TESTING.md](TESTING.md) for the full breakdown

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
