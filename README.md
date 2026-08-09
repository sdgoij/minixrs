# minixrs

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

> **⚠️ Research project — not production-ready.**  
> If you're looking for a production operating system, use Linux, a BSD, or [Redox](https://www.redox-os.org/) instead.

## Recent work

The last few days moved the project from "boots a shell" to "a real toolchain target":

- **1:1 kernel threads** — every thread is a schedulable Proc slot: `thread_create`/`exit`/`join`/`yield`, wake-one IPC delivery, group sweep on exit/exec/fork, per-thread TLS. MINIX proper had no native threads; this port does.
- **A working `std` port** — the forked rustc's std PAL for minix (`sys/pal/minix`) runs on the OS: `/bin/hello` is a std-linked binary that spawns threads with TLS and exits cleanly.
- **Networking that works** — virtio-net plus DNS: `/bin/udp nos.nl` resolves hostnames from inside QEMU.
- **Memory from 72M to 16G** — the same kernel boots in ~72 MiB of guest RAM and runs `/bin/hello` up to 16 GiB, on all three architectures (x86_64, RISC-V64, AArch64).
- **A heap that actually grows** — userland heap growth routed through VM's brk (demand-mapped, freed on exit); the COW refcount bug that killed repeated `hello` runs is fixed.
- **Honest memory reporting** — the boot banner prints detected vs usable RAM (a 4 GiB guest says `4095 MiB detected (4078 MiB usable)`, not the old "5120 MiB" artifact).
- **uutils/coreutils builds for minix** — the `echo` util compiles and links for `x86_64-pc-minix` against the fork's std (the `coreutils` submodule tracks the port; not yet booted on the OS).

## Quick Start

### Prerequisites

- Rust toolchain (MSRV: **1.96**, edition: **2024**) — the OS is built with
  the forked Rust compiler in the `rust/` submodule; `just bootstrap` builds
  its stage1 compiler + the minix std sysroots (first run needs network)
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
# One-time setup: fetch the rust fork submodule, build its stage1 compiler
# + std for all minix targets, and the /bin/hello std smoke test
just bootstrap

# x86_64
just build                    # Build the kernel + boot images
just run                      # Build and boot in QEMU
just debug                    # Build and boot with GDB server on :1234
just test-qemu                # Run the QEMU integration tests

# RISC-V64
just build riscv64            # Build the RISC-V kernel
just run riscv64              # Boot in QEMU (uses OpenSBI)
just test-qemu riscv64        # Run the QEMU integration tests
just test-boot riscv64        # Run the boot tests

# AArch64
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
  - `just test-qemu` (x86_64) — 84 tests, exits with a real pass/fail code
  - `just test-qemu riscv64` — 70 tests, paging enabled
  - `just test-qemu aarch64` — 70 tests, MMU enabled

  RISC-V/AArch64 integration builds enable the MMU before running the shared
  suite, so copy_from_user / delivermsg perform real page-table walks, the
  SENDREC payload assertions run on all three arches, and per-arch hardware
  tests probe the actual devices: RISC-V CLINT timer (rdtime + SSTC
  stimecmp) and SBI console; AArch64 generic timer (cntpct_el0/cntfrq_el0)
  and PL011 UART.
- **Boot tests:** `just test-boot [arch]` — multi-server verification after VFS
  mount_root on all three arches (server liveness, process-table consistency,
  VFS→MFS readsuper IPC round-trip, brk/RAM-disk mappings, allocator, initramfs)

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
- All three arches build with the fork's stage1 compiler and in-tree minix
  targets (`x86_64-pc-minix`, `riscv64gc-unknown-minix`, `aarch64-unknown-minix`)
  — no `-Zbuild-std`, no JSON specs. `just bootstrap` builds the compiler once.

## License

Licensed under the [GNU General Public License v2.0](LICENSE.md).

MINIX 3 source code references are used under the [LICENSE.MINIX](LICENSE.MINIX).
