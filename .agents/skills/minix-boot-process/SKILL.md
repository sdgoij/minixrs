---
name: minix-boot-process
description: The MINIX/Rust boot sequence from QEMU reset to shell prompt. Use when working on boot initialization, process loading, server startup, or debugging why a server isn't running.
---

# MINIX/Rust Boot Process

## Boot Chain Overview

```
QEMU → SeaBIOS → multiboot ROM → trampoline.elf (i386)
  → kernel.bin (x86_64 at 0x200000) → kmain() → boot_init()
  → server processes (PM, VFS, MFS, etc.) → init → shell
```

## Stage 1: Trampoline (trampoline.S)

File: `crates/kernel-boot/src/trampoline.S`

The ELF32 multiboot trampoline transitions the CPU from 32-bit to 64-bit long mode:

1. Saves multiboot args (magic in EDI, info in ESI)
2. Checks CPUID + long mode support
3. Sets up identity-mapped page tables: PML4[0] → PDP[0] → PD[0..511] (512×2MB = 1GB)
4. Enables PAE (CR4.PAE + CR4.PGE), loads CR3, enables Long Mode (EFER.LME)
5. Enables paging (CR0.PG | CR0.WP), loads 64-bit GDT
6. Far-jumps (lret) to 64-bit entry, sets up temporary stack
7. Jumps to kmain() at address extracted by mkboot

Built by `build.rs` using clang + rust-lld. Address determined by mkboot extracting `kmain` symbol.

## Stage 2: kmain() (crates/kernel-boot/src/main.rs)

Order of initialization (each must succeed before the next):

```
kmain()
  ├─ Enable SSE (CR4.OSFXSR | OSXMMEXCPT)
  ├─ kernel::init() — GDT, IDT, FPU, APIC, page allocator, cpulocals
  ├─ kernel::syscall::init_basic_syscalls() — syscall handlers (0..63)
  ├─ dma::register_allocator() — phys allocator for DMA
  ├─ init_serial() — COM1 (0x3F8) at 115200 baud
  ├─ arch_x86_64::alloc::init_allocator() — phys pool 0x300000–0x10000000
  ├─ serial_write("Hello MINIX!\r\n")
  ├─ PIT timer init (100 Hz): remap PIC, program PIT, set ISR, unmask IRQ0
  ├─ [integration-tests] → test_runner::run_integration_tests()
  └─ [normal boot] → boot_init()
```

## Stage 3: Server Loading (boot_init)

File: `crates/kernel-boot/src/boot_init.rs`

The kernel loads 8 server binaries from the embedded **initramfs** (CPIO archive):

| Process | Endpoint | Description |
|---------|----------|-------------|
| PM | 3 | Process Manager — fork, exec, exit, signals |
| RS | 4 | Reincarnation Server — process restart, live update |
| VFS | 5 | Virtual File System — file I/O dispatch |
| VM | 6 | Virtual Memory — page faults, mmap |
| RAMDISK | — | Block device feeding the MINIX FS image to MFS |
| MFS | 7 | Minix File System — reads/writes the on-disk MINIX FS |
| DS | 9 | Data Store — key-value storage for system metadata |
| TTY | 10 | Teletype — serial/console input handling |
| SCHED | 11 | Scheduler — scheduling policies |
| INIT | 12 | Init — starts the shell |

### Loading Sequence

For each boot process:
1. `find_initramfs_file()` — locate ELF binary in CPIO archive
2. `calc_elf_bounds()` — parse ELF, compute page count
3. `vm::alloc_mem(code_pages, 0)` — allocate unique physical pages
4. `load_elf_at()` — load ELF segments into pages at phys_base + (vaddr - elf_base)
5. `vm::alloc_mem(stack_pages)` — allocate unique stack pages
6. `setup_user_stack()` — set up argv at identity-mapped stack VA
7. Copy stack data to per-process physical pages
8. Set TrapFrame (rcx=RIP, r11=RFLAGS, rsp) in Proc entry

Then for each process: `boot_create_restricted_page_table(...)` — creates per-process page table.

Finally: enqueue all processes, set cpulocal proc pointer, and call `restore(first_proc)` to switch to PM.

## Stage 4: Server Startup

Servers start executing their `_start` entry point (provided by `minix-rt`), which calls their `main()` function:

- **PM** (`crates/servers/src/pm.rs`): `pm_server_main()` — initializes process table, then enters main loop receiving IPC from other processes (fork, exec, exit requests)
- **VFS** (`crates/servers/src/vfs/main.rs`): `vfs_main()` — calls `sef_local_startup()` (syscall-based SEF init), then enters `get_work() / handle_work()` loop
- **MFS** (`crates/fs/src/mfs/main.rs`): `mfs_main()` — initializes buffer cache + RAM disk I/O, then enters receive/dispatch loop

### RAM Disk for MFS

The kernel maps the MINIX FS image (`minixfs_data.rs` → `.minixfs` section) into MFS's address space at `MFS_RAMDISK_VA`. MFS initializes a `BlockIoFn` callback (`ram_disk_io`) that reads/writes blocks from this memory region.

## Stage 5: VFS Root Mount

VFS's `sef_cb_init_fresh()` (simplified from C):
1. Manually initializes `fproc[]` table
2. Sets PM's entry: `fproc[0].fp_endpoint = PM_PROC_NR`
3. Calls `init_dmap()` then `mount_root()`
4. `mount_root()` calls `req_readsuper(MFS_PROC_NR, dev=0)` — the RAM disk
5. MFS reads superblock from RAM disk, loads root inode, replies
6. VFS populates root vnode, sets `fp_rdir` / `fp_cdir` for boot processes
7. VFS enters main loop

The full PM↔VFS boot protocol (VFS_PM_INIT handshake) is **not implemented** in the Rust version. See BLOCKERS.md for details.

## Key Files

| File | Role |
|------|------|
| `crates/kernel-boot/src/trampoline.S` | 32-bit → 64-bit transition |
| `crates/kernel-boot/src/main.rs` | kmain(), syscall handlers, boot test runner |
| `crates/kernel-boot/src/boot_init.rs` | Server loading, page table creation |
| `crates/kernel-boot/src/boot_test.rs` | In-kernel boot verification tests |
| `tools/mkboot.rs` | Build orchestrator (trampoline + kernel + initramfs) |
| `tools/mkinitramfs.rs` | CPIO archive builder |
| `tools/mkminixfs.rs` | MINIX FS image builder |
| `tools/minix-raw.ld` | Kernel linker script (link address 0x200000) |

## Build & Run

```
just build          # Build kernel + trampoline + initramfs
just run            # Boot QEMU with serial console
just test-boot      # Build + boot + run 12 assertion tests + exit
```
