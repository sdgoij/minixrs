# Minix 3.3.0 → Rust Porting Plan

## Executive Summary

This plan describes a phased, incremental port of the [Minix 3.3.0](https://www.minix3.org/) microkernel operating system from C to Rust. Minix 3.3.0 targets x86 (i386) and ARM. The Rust port adds **x86_64 as the primary target** (not just i386) with **RISC-V64 as a bonus second architecture**.

The port is composed of:

| Layer | What it is | C source | Status |
|-------|-----------|----------|--------|
| **Kernel arch (x86_64)** | Boot, paging, interrupts, registers, context switch, syscalls | Adapted from `.refs/minix-3.3.0/sys/arch/i386/` + `.refs/minix-3.3.0/sys/arch/x86/` | New (no x86_64 in Minix 3.3.0) |
| **Kernel arch (RISC-V)** | Same layer for RISC-V64 | `.refs/minix-3.3.0/sys/arch/evbarm/` as reference | Bonus challenge |
| **Kernel core** | Process table, scheduling, IPC, syscalls | `.refs/minix-3.3.0/minix/kernel/` | Port |
| **VM server** | Page tables, memory management | `.refs/minix-3.3.0/minix/servers/vm/` | Port |
| **VFS server** | Virtual filesystem | `.refs/minix-3.3.0/minix/servers/vfs/` | Port |
| **Drivers** | Block, char, net, USB, audio, etc. | `.refs/minix-3.3.0/minix/drivers/` | Port |
| **System servers** | SCHED, RS, PM, DS, IPC, DEVMAN | `.refs/minix-3.3.0/minix/servers/` | Port |
| **Filesystems** | MINIX FS, ext2, MFS, etc. | `.refs/minix-3.3.0/minix/fs/` | Port |
| **Userland** | ~145 commands | `.refs/minix-3.3.0/bin/`, `usr.bin/`, `usr.sbin/`, `sbin/`, `minix/commands/` | Port |
| **Libraries** | libc, libm, libutil, libz, etc. | `.refs/minix-3.3.0/lib/`, `.refs/minix-3.3.0/minix/lib/` | Port |
| **Network stack** | TCP/IP, UDP, IP, ARP | `.refs/minix-3.3.0/sys/net/`, `netinet/`, `netinet6/` | Port |

- A **kernel** (`sys/arch/*/compile/GENERIC` + `minix/kernel/`)
- **System servers** as user-space processes (VM, VFS, SCHED, RS, PM, DS, IPC, TTY)
- **Device drivers** (~30+ driver modules)
- **File system modules** (MINIX FS, ext2, MFS, procfs, iso9660fs, vbfs, hgfs)
- **Userland** (NetBSD-derived `bin/`, `usr.bin/`, `usr.sbin/`, `sbin/`, `gnu/`, `external/`)
- **C standard library** (`lib/libc/`) and supporting libraries (`lib/libm`, `lib/libz`, `lib/libutil`, etc.)
- **Driver libraries** (`minix/lib/lib*driver*`)
- **Network stack** (`sys/net/`, `sys/netinet/`, `sys/netinet6/`)

The port preserves the **entire architectural design** — message-passing IPC, privilege separation, grant-based memory sharing, capability-based I/O permissions — but rewrites the implementation in Rust. The target is a **1:1 functional equivalent** running on the same x86 (and optionally ARM) hardware.

## Project Convention

- **Rust minimum version**: 1.96 (stable)
- **Edition**: 2024
- **Workspace layout**: all crates live under `./crates/`
- **Source reference**: every task references the exact file path(s) in `.refs/minix-3.3.0/`
- **Testing**: every task has a corresponding test obligation

## No Stubs — Real Implementations Only

**Write real code. Do not stub out functionality with `unimplemented!()`, `panic!("not yet")`, or empty `todo!()` calls.**

Every function, method, and module you touch must do something meaningful. If you are implementing a feature that requires infrastructure that does not yet exist, do one of two things:

1. **Implement the missing infrastructure first** — this becomes the prerequisite task.
2. **If you cannot implement it in this session**, add a **new task** to this tracker describing the missing functionality, then use `todo!("<brief explanation of what goes here>")` with the task reference so future agents know what to implement.

### Good `todo!()` examples

```rust
// GOOD — explains what, why, and links to the tracker
todo!("Read config from user's shell preference; see NEXT.md T3.1");

// GOOD — clear scope for the future task
todo!("Implement ConPTY backend for Windows Container PTY; see NEXT.md T0.1 follow-up");
```

### Bad `todo!()` examples

```rust
// BAD — no explanation
todo!();

// BAD — vague
todo!("implement later");

// BAD — stub with empty body
fn some_method(&self) {
    // TODO: implement
}
```

**Rule of thumb:** if the code compiles but the behavior is a no-op, it's a stub. Stubs are only allowed when explicitly marked with `todo!()` + a new task reference.

---

## Testing Requirement (MANDATORY)

**Every task MUST include tests. No exceptions.**

| Test Type | When to Use | Example |
|-----------|-------------|---------|
| **Unit tests** (`#[test]`) | Pure functions, state machines, parsing, configuration | VT SGR parsing, buffer row operations, keybinding matching |
| **Property tests** (`proptest`) | Any input where behavior should hold for ALL valid inputs | VT sequence parser handles arbitrary escape sequences, buffer invariant (rows/cols always valid after any operation), keybinding always finds at most one match |
| **Integration tests** | Multi-crate behavior, widget tree, event dispatch | Connection → VT adapter → TextBuffer pipeline, tab creation flow |

---

## IMPORTANT: Agent Behavior

### Git — DO NOT TOUCH STAGED FILES
The operator controls all staging and commit decisions. **Agents must never stage or unstage anything.**
- **NEVER** run `git add` — the operator decides what gets staged.
- **NEVER** run `git commit` — the operator decides what gets committed and with what message.
- The operator reviews **everything** before it enters the repo history.

---

## Phase 0: Project Structure & Build System

**Goal**: Establish the Rust project scaffolding and build system before touching any code.

### Targets

The Rust port targets two architectures:

| Target | Custom JSON spec | Notes |
|--------|-----------------|-------|
| **x86_64-pc-minix** (primary) | `x86_64-pc-minix.json` | 64-bit x86, UEFI or multiboot2. This is the main delivery target. |
| **riscv64gc-unknown-minix** (bonus) | `riscv64gc-unknown-minix.json` | RISC-V64 with G extension (GC = IMACFD). Requires full arch layer from scratch. |

### Tasks

- [x] **0.1 — Create workspace layout**
  - Path: `minixrs/` (project root)
  - Structure:
    ```
    minixrs/
    ├── Cargo.toml                  # workspace root (edition = "2024", rust-version = "1.96")
    ├── x86_64-pc-minix.json        # x86_64 custom target (primary)
    ├── riscv64gc-unknown-minix.json # RISC-V64 GC custom target (bonus)
    ├── crates/
    │   ├── arch-common/            # arch-independent kernel primitives
    │   ├── arch-x86_64/            # x86_64-specific kernel code
    │   ├── arch-riscv64/           # RISC-V64-specific kernel code (bonus)
    │   ├── drivers/                # driver framework + individual drivers
    │   ├── fs/                     # filesystem crates
    │   ├── kernel/                 # core kernel (processes, scheduling, IPC, VM)
    │   ├── libs/                   # libc, libm, libutil, etc.
    │   ├── net/                    # networking stack
    │   ├── servers/                # system server crates
    │   └── userland/               # userland command binaries
    └── tools/                      # build tools, linker scripts
    ```
  - Test: `cargo build` succeeds for the empty workspace
  - Test: `cargo test` runs without errors (no-op)
  - Source: N/A (creation task)

- [x] **0.2 — Set edition = "2024" + MSRV**
  - Set `edition = "2024"` and `rust-version = "1.96"` in every crate's `Cargo.toml`
  - Test: `cargo metadata` succeeds
  - Source: N/A (configuration task)

- [x] **0.3 — Set up cross-compilation for x86 Minix target**
  - Custom JSON target specification (`x86_64-pc-minix`)
  - Linker: `rust-lld` with custom linker script (`tools/minix-raw.ld`)
  - Multiboot 2 bootloader compatibility
  - Target features: `mmx`, `sse`, `sse2`, `sysenter` (x86)
  - Test: `cargo build --target x86_64-pc-minix.json` produces a valid ELF object
  - Test: Linker script correctly places `.multiboot` section
  - Source: `sys/arch/i386/stand/` (bootloader), `sys/arch/i386/conf/GENERIC` (config)

- [x] **0.4 — Define crate dependency graph**
  ```
  arch-common            # arch-independent low-level primitives
  ├── arch-x86_64        # x86_64-specific low-level (registers, interrupts, page tables)
  ├── arch-riscv64       # RISC-V64-specific low-level
  ├── drivers            # driver framework traits & abstractions
  ├── fs                 # filesystem crates
  ├── kernel             # core kernel (processes, scheduling, IPC, VM)
  ├── net                # networking stack
  ├── servers            # SEF, syslib for user-space servers
  ├── libs               # libc, libm, libutil re-implementation
  └── userland           # individual userland binaries
  ```
  - Test: `cargo tree` shows correct dependency graph
  - Source: N/A (planning task)

- [x] **0.5 — Bootable kernel binary + QEMU launch** (partial: kmain + serial + panic handler done)
  - [x] `crates/kernel-boot/` — boot binary crate (breaks circular dep between kernel and arch-x86_64)
  - [x] `kmain()` — serial init (inline asm, 115200 baud), print banner, `hlt_loop()`
  - [x] `#[panic_handler]` — HLT loop on panic
  - [x] Builds with `cargo build -p kernel-boot --target x86_64-unknown-none`
  - [ ] `_start` in `naked_asm!` — 32→64 bit transition with multiboot1 header, identity paging
  - **Two entry paths:**
    - `boot_entry::_start` — standalone multiboot1 entry (32→64 transition, identity paging, calls `kmain`)
    - `crates/kernel-boot/trampoline.S` + `crates/kernel-boot/trampoline.ld` — ELF32 multiboot trampoline (qboot), does 32→64 transition, jumps to `kmain`
  - `kmain()` — simplified: serial init (inline asm on COM1, 115200 baud), print banner + "Hello MINIX!", `hlt_loop()`
  - `#[panic_handler]` + `print!`/`println!` via serial (COM1, 115200 baud)
  - `crates/kernel-boot/build.rs` — assembles + links trampoline automatically during `cargo build`
  - `tools/minix-raw.ld` — kernel linked at 0x200000 for `-device loader`; includes `.got`/`.got.plt`
    sections for `code-model=kernel` PIC support; `.text.kmain` for deterministic placement
  - `kernel_entry` in `trampoline.S` updated to match `kmain` address (verify with `rust-nm`)
  - **Third entry path — bootable disk image (SeaBIOS):**
    - `tools/mbr.S` — MBR bootloader (stage1, 512 bytes), loads stage2 from disk, jumps to 0x1000
    - `tools/stage2.S` — stage2 bootloader (loaded at 0x1000), reads kernel from disk via INT 13h,
      transitions through real→protected→long mode, copies kernel to 0x200000, jumps to `kmain`
    - `tools/mkimg.rs` — Rust image builder: compiles mbr.S + stage2.S with clang/rust-lld,
      extracts kmain address from kernel ELF via rust-nm, patches stage2, creates 8MB disk image
    - `just image` — `just build` + `rustc tools/mkimg.rs` → `target/minix.img`
    - `just run-img` — `qemu-system-x86_64 ... -drive format=raw,file=target/minix.img`
    - Boots via default SeaBIOS (no special BIOS needed), outputs clean banner + "Hello MINIX!"
  - `Justfile` — `just build`, `just run` (qboot BIOS), `just image` (disk image), `just run-img` (disk boot)
  - `tools/` cleaned up: only `minix-raw.ld` (kernel linker script), `mbr.S` (MBR), `stage2.S` (stage2),
    and `mkimg.rs` (image builder) remain
  - Compiler builtins + BSS clearing via linker symbols
  - Serial uses inline asm directly (avoids function pointer corruption under `code-model=kernel`)
  - QEMU exits cleanly after `hlt`
  - Test: Verify the task outcome with /

---

## Phase 1: Foundation — Kernel Types & ABI Compatibility

**Goal**: Define all Rust types that mirror the C types exactly, ensuring ABI compatibility for the IPC message protocol, process table, and kernel-user boundary.

> **Critical**: Every type must be verified with compile-time `const _: () = assert!(...)` blocks checking both `size_of::<T>()` and `offset_of!()` for every field. These are stricter than the C header's `_ASSERT_MSG_SIZE()` because they verify field offsets, not just struct size.

### Tasks

- [x] **1.1 — Port `minix/type.h` → Rust types**
  - Source: `.refs/minix-3.3.0/minix/include/minix/type.h`
  - Types: `vir_bytes`, `phys_bytes`, `phys_clicks`, `vir_clicks`, `endpoint_t`, `cp_grant_id_t`
  - Structs: `vir_addr`, `vir_cp_req`, `vumap_vir`, `vumap_phys`, `iovec_t`, `iovec_s_t`, `sigmsg`
  - Structs: `loadinfo`, `machine`, `io_range`, `minix_mem_range`, `boot_image`, `memory`
  - Structs: `kmessages`, `k_randomness`, `minix_kerninfo`
  - All marked `#[repr(C)]`, `#[repr(packed)]` where C uses `__packed`
  - Tests: `static_assert!(size_of::<vir_addr>() == X);`
  - Tests: `static_assert!(size_of::<message>() == 56);`
  - Tests: Compile-time size verification for every struct

- [x] **1.2 — Port `minix/const.h` constants**
  - Source: `.refs/minix-3.3.0/minix/include/minix/const.h`
  - Constants: `NR_PROCS`, `NR_TASKS`, `NR_SYS_PROCS`, `NR_MEMS`, `CLICK_SIZE`, `CLICK_SHIFT`, `NR_CONS`, `NR_RS_LINES`, `NR_PTYS`, `NR_SCHED_QUEUES`, `NR_IO_RANGE`, `NR_MEM_RANGE`, `NR_IRQ`
  - Constants: `MAX_INODE_NR`, `MAX_FILE_POS`, `UMAX_FILE_POS`, `MAX_SYM_LOOPS`
  - File mode bits: `I_TYPE`, `I_UNIX_SOCKET`, `I_SYMBOLIC_LINK`, `I_REGULAR`, `I_BLOCK_SPECIAL`, `I_DIRECTORY`, `I_CHAR_SPECIAL`, `I_NAMED_PIPE`, `I_SET_UID_BIT`, `I_SET_GID_BIT`, `I_SET_STCKY_BIT`, `ALL_MODES`, `RWX_MODES`, `R_BIT`, `W_BIT`, `X_BIT`
  - Constants: `PMAGIC`, `NO_BLOCK`, `NO_ENTRY`, `NO_ZONE`, `NO_DEV`, `NO_LINK`
  - Constants: `PREEMPTIBLE`, `BILLABLE`, `DYN_PRIV_ID`, `SYS_PROC`, `CHECK_IO_PORT`, `CHECK_IRQ`, `CHECK_MEM`, `ROOT_SYS_PROC`, `VM_SYS_PROC`
  - Constants: `VM_D`, `VM_GRANT`, `PHYS_SEG`, `SEGMENT_TYPE`, `SEGMENT_INDEX`
  - Constants: `VERBOSEBOOT_*` values
  - Constants: `MKF_I386_INTEL_SYSENTER`, `MKF_I386_AMD_SYSCALL`
  - Tests: Every constant value matches the C `#define` value (diff the C and Rust sources)
  - Tests: `assert_eq!(CLICK_SIZE, 4096)`

- [x] **1.3 — Port `minix/ipcconst.h` constants**
  - Source: `.refs/minix-3.3.0/minix/include/minix/ipcconst.h`
  - IPC call numbers: `SEND` (1), `RECEIVE` (2), `SENDREC` (3), `NOTIFY` (4), `SENDNB` (5), `MINIX_KERNINFO` (6), `SENDA` (16), `IPCNO_HIGHEST`
  - Status macros: `IPC_STATUS_CALL_SHIFT`, `IPC_STATUS_CALL_MASK`, `IPC_STATUS_CALL()`, `IPC_STATUS_CALL_TO()`, `IPC_STATUS_FLAGS_SHIFT`, `IPC_STATUS_FLAGS()`, `IPC_STATUS_FLAGS_TEST()`
  - `IPC_FLG_MSG_FROM_KERNEL`
  - Tests: `static_assert!(size_of::<message>() == 56);`
  - Tests: `assert_eq!(SEND, 1); assert_eq!(RECEIVE, 2); ...`
  - Tests: `IPC_STATUS_CALL(IPC_STATUS_CALL_TO(5)) == 5`

- [x] **1.4 — Port `minix/com.h` — the single most important header**
  - Source: `.refs/minix-3.3.0/minix/include/minix/com.h`
  - Subsystem process endpoints: `IDLE`, `CLOCK`, `SYSTEM`, `KERNEL`, `HARDWARE`, `MAX_NR_TASKS`, `NR_TASKS`
  - Special process numbers: `PM_PROC_NR`, `VFS_PROC_NR`, `RS_PROC_NR`, `MEM_PROC_NR`, `SCHED_PROC_NR`, `TTY_PROC_NR`, `DS_PROC_NR`, `MFS_PROC_NR`, `VM_PROC_NR`, `PFS_PROC_NR`, `LAST_SPECIAL_PROC_NR`
  - Process limits: `INIT_PROC_NR`, `NR_BOOT_MODULES`, `ROOT_SYS_PROC_NR`, `ROOT_USR_PROC_NR`
  - Notification: `NOTIFY_MESSAGE`, `is_ipc_notify()`, `is_notify()`, `is_ipc_asynch()`
  - PCI bus control: `BUSC_RQ_BASE`, `BUSC_RS_BASE`, `BUSC_PCI_INIT`, `BUSC_PCI_FIRST_DEV`, `BUSC_PCI_NEXT_DEV`, `BUSC_PCI_FIND_DEV`, `BUSC_PCI_IDS`, `BUSC_PCI_RESERVE`, `BUSC_PCI_ATTR_R8/16/32`, `BUSC_PCI_ATTR_W8/16/32`, `BUSC_PCI_RESCAN`, `BUSC_PCI_DEV_NAME_S`, `BUSC_PCI_SLOT_NAME_S`, `BUSC_PCI_SET_ACL`, `BUSC_PCI_DEL_ACL`, `BUSC_PCI_GET_BAR`
  - I2C: `BUSC_I2C_RESERVE`, `BUSC_I2C_EXEC`
  - Driver layer: `DL_RQ_BASE`, `DL_RS_BASE`, `IS_DL_RQ()`, `IS_DL_RS()`, `DL_CONF`, `DL_GETSTAT_S`, `DL_WRITEV_S`, `DL_READV_S`, `DL_READV_S`, `DL_CONF_REPLY`, `DL_STAT_REPLY`, `DL_TASK_REPLY`, `DL_NOFLAGS`, `DL_PACK_SEND`, `DL_PACK_RECV`, `DL_NOMODE`, `DL_PROMISC_REQ`, `DL_MULTI_REQ`, `DL_BROAD_REQ`
  - System calls: `KERNEL_CALL`, all `SYS_*` constants (~50 syscalls)
  - I/O types: `_DIO_INPUT`, `_DIO_OUTPUT`, `_DIO_DIRMASK`, `_DIO_BYTE`, `_DIO_WORD`, `_DIO_LONG`, `_DIO_TYPEMASK`, `_DIO_SAFE`, `_DIO_SAFEMASK`, all `DIO_*` constants
  - IRQ: `IRQ_SETPOLICY`, `IRQ_RMPOLICY`, `IRQ_ENABLE`, `IRQ_DISABLE`, `IRQ_REENABLE`, `IRQ_BYTE`, `IRQ_WORD`, `IRQ_LONG`
  - Copy flags: `CP_FLAG_TRY`
  - GETINFO: `GET_KINFO`, `GET_IMAGE`, `GET_PROCTAB`, `GET_RANDOMNESS`, `GET_MONPARAMS`, `GET_KENV`, `GET_IRQHOOKS`, `GET_PRIVTAB`, `GET_KADDRESSES`, `GET_SCHEDINFO`, `GET_PROC`, `GET_MACHINE`, `GET_LOCKTIMING`, `GET_BIOSBUFFER`, `GET_LOADINFO`, `GET_IRQACTIDS`, `GET_PRIV`, `GET_HZ`, `GET_WHOAMI`, `GET_RANDOMNESS_BIN`, `GET_IDLETSC`, `GET_CPUINFO`, `GET_REGS`, `GET_RUSAGE`
  - Privilege: `SYS_PRIV_ALLOW`, `SYS_PRIV_DISALLOW`, `SYS_PRIV_SET_SYS`, `SYS_PRIV_SET_USER`, `SYS_PRIV_ADD_IO`, `SYS_PRIV_ADD_MEM`, `SYS_PRIV_ADD_IRQ`, `SYS_PRIV_QUERY_MEM`, `SYS_PRIV_UPDATE_SYS`, `SYS_PRIV_YIELD`
  - VFS PM: `VFS_PM_*` message types (all variants)
  - VM: `VM_BASE`, `VM_RQ_BASE`, `VM_EXIT`, `VM_FORK`, `VM_BRK`, `VM_EXEC_NEWMEM`, `VM_WILLEXIT`, `VM_MMAP`, `VM_MUNMAP`, `VM_ADDDMA`, `VM_DELDMA`, `VM_GETDMA`, `VM_MAP_PHYS`, `VM_UNMAP_PHYS`, `VM_MAPCACHEPAGE`, `VM_SETCACHEPAGE`, `VM_CLEARCACHE`, `VM_VFSREQ_*`, `VMVFSREQ_*`, `VM_REMAP`, `VM_SHM_UNMAP`, `VM_GETPHYS`, `VM_GETREF`, `VM_RS_SET_PRIV`, `VM_QUERY_EXIT`, `VM_NOTIFY_SIG`, `VM_INFO`, `VMIW_*`, `VM_RS_UPDATE`, `VM_RS_MEMCTL`, `VM_WATCH_EXIT`, `VM_REMAP_RO`, `VM_PROCCTL`, `VMPCTL_*`, `VM_BASIC_CALLS`, `VM_PAGEFAULT`, `VM_CALL_MASK_SIZE`
  - IPC: `IPC_BASE`, `IPC_SHMGET`, `IPC_SHMAT`, `IPC_SHMDT`, `IPC_SHMCTL`, `IPC_SEMGET`, `IPC_SEMCTL`, `IPC_SEMOP`
  - Scheduling: `SCHEDULING_BASE`, `SCHEDULING_NO_QUANTUM`, `SCHEDULING_START`, `SCHEDULING_STOP`, `SCHEDULING_SET_NICE`, `SCHEDULING_INHERIT`
  - USB: `USB_BASE`, all `USB_*` constants
  - DEVMAN: `DEVMAN_BASE`, `DEVMAN_ADD_DEV`, `DEVMAN_DEL_DEV`, `DEVMAN_ADD_BUS`, `DEVMAN_DEL_BUS`, `DEVMAN_ADD_DEVFILE`, `DEVMAN_DEL_DEVFILE`, `DEVMAN_REQUEST`, `DEVMAN_REPLY`, `DEVMAN_BIND`, `DEVMAN_UNBIND`
  - TTY: `TTY_RQ_BASE`, `TTY_FKEY_CONTROL`, `FKEY_MAP`, `FKEY_UNMAP`, `FKEY_EVENTS`, `TTY_INPUT_UP`, `TTY_INPUT_EVENT`
  - INPUT: `INPUT_RQ_BASE`, `INPUT_RS_BASE`, `INPUT_CONF`, `INPUT_SETLEDS`, `INPUT_EVENT`
  - VFS transaction: `VFS_TRANSACTION_BASE`, `VFS_TRANSID`, `IS_VFS_FS_TRANSID()`
  - CDEV: `CDEV_RQ_BASE`, `CDEV_RS_BASE`, `IS_CDEV_RQ()`, `IS_CDEV_RS()`, `CDEV_OPEN`, `CDEV_CLOSE`, `CDEV_READ`, `CDEV_WRITE`, `CDEV_IOCTL`, `CDEV_CANCEL`, `CDEV_SELECT`, `CDEV_REPLY`, `CDEV_SEL1_REPLY`, `CDEV_SEL2_REPLY`, `CDEV_R_BIT`, `CDEV_W_BIT`, `CDEV_NOCTTY`, `CDEV_NOFLAGS`, `CDEV_NONBLOCK`, `CDEV_OP_RD`, `CDEV_OP_WR`, `CDEV_OP_ERR`, `CDEV_NOTIFY`, `CDEV_CLONED`, `CDEV_CTTY`
  - BDEV: `BDEV_RQ_BASE`, `BDEV_RS_BASE`, `IS_BDEV_RQ()`, `IS_BDEV_RS()`, `BDEV_OPEN`, `BDEV_CLOSE`, `BDEV_READ`, `BDEV_WRITE`, `BDEV_GATHER`, `BDEV_SCATTER`, `BDEV_IOCTL`, `BDEV_REPLY`, `BDEV_R_BIT`, `BDEV_W_BIT`, `BDEV_NOFLAGS`, `BDEV_FORCEWRITE`, `BDEV_NOPAGE`
  - RTC: `RTCDEV_RQ_BASE`, `RTCDEV_RS_BASE`, `IS_RTCDEV_RQ()`, `IS_RTCDEV_RS()`, `RTCDEV_GET_TIME`, `RTCDEV_SET_TIME`, `RTCDEV_PWR_OFF`, `RTCDEV_GET_TIME_G`, `RTCDEV_SET_TIME_G`, `RTCDEV_REPLY`, `RTCDEV_NOFLAGS`, `RTCDEV_Y2KBUG`, `RTCDEV_CMOSREG`
  - `struct message` — the central IPC message union (56 bytes)
  - `COMMON_RQ_BASE`, `SIGS_SIGNAL_RECEIVED`, `COMMON_REQ_GCOV_DATA`, `COMMON_REQ_FI_CTL`
  - Tests: Every constant value matches the C `#define` value
  - Tests: `static_assert!(size_of::<message>() == 56);`
  - Tests: `assert_eq!(NR_TASKS, 8);` (or whatever the config defines)
  - Tests: Diff Rust enum variants against C enum/define values

- [x] **1.5 — Port `minix/endpoint.h`**
  - Source: `.refs/minix-3.3.0/minix/include/minix/endpoint.h`
  - Endpoint numbering scheme, generation logic
  - Tests: Endpoint resolution returns correct values for known constants

- [x] **1.6 — Port `minix/ipc.h`**
  - Source: `.refs/minix-3.3.0/minix/include/minix/ipc.h`
  - `Message` struct (m_source, m_type, m_payload union)
  - `MessageUnion` with 62 payload variants (mess_u8 through mess_vmmcp_reply)
  - `DsVal` union (cp_grant_id_t / u32 / endpoint_t)
  - `AsynMsg` struct with AMF_* flags
  - `MinixIpcVecs` IPC function vector with 7 function pointer types
  - Field access constants (M1_I1 through M10_ULL1) via `offset_of!`
  - Tests: `size_of::<Message>() >= 64` (platform-dependent alignment)
  - Tests: All 62 union variants present and match C layout

- [x] **1.7 — Port `minix/sys_config.h`**
  - Source: `.refs/minix-3.3.0/minix/include/minix/sys_config.h`
  - `config.rs`: FP_FORMAT, FP_NONE, FP_IEEE, DEBUG_LOCK_CHECK, DEFAULT_STACK_LIMIT
  - `NR_PROCS`, `NR_SYS_PROCS` in `endpoint.rs` (used by endpoint calculations)
  - `KMESS_BUF_SIZE` in `types.rs` (used by KMessages struct)
  - `CLICK_SIZE`, `NR_MEMS`, `MAX_INODE_NR`, `MAX_FILE_POS`, `UMAX_FILE_POS`, `MAX_SYM_LOOPS` in `consts.rs` (task 1.2)
  - Tests: Unit tests for each type/function; compile-time size/offset assertions where applicable

- [x] **1.8 — Port `minix/safecopies.h`**
  - Source: `.refs/minix-3.3.0/minix/include/minix/safecopies.h`
  - `safecopies.rs`: CpGrant (cp_grant_t), CpUnion with 3 variants (direct/indirect/magic)
  - `VscpVec` struct (32 bytes) for vectored safecopy descriptors
  - Constants: `GRANT_INVALID`, `grant_valid()`, `CPF_READ` through `CPF_VALID` (8 flags)
  - 10 function prototypes with `extern "C"` stub signatures
  - Compile-time size checks: `size_of::<CpGrant>() >= 36`, `size_of::<VscpVec>() >= 32`
  - Tests: Unit tests for each type/function; compile-time size/offset assertions where applicable

- [x] **1.9 — Port `minix/vm.h`**
  - Source: `.refs/minix-3.3.0/minix/include/minix/vm.h`
  - `vm.rs`: VmStatsInfo, VmUsageInfo, VmRegionInfo structs
  - Constants: `MVM_WRITABLE`, `VMPTYPE_NONE`, `VMPTYPE_CHECK`, `MAX_VRI_COUNT`, VMMC_* flags, `VMC_NO_INODE`
  - 24 function prototypes with `extern "C"` stub signatures
  - `vm_exit`, `vm_fork`, `vm_willexit`, `vm_adddma`, `vm_deldma`, `vm_getdma`
  - `vm_map_phys`, `vm_unmap_phys`, `vm_notify_sig`, `vm_set_priv`, `vm_update`
  - `vm_memctl`, `vm_query_exit`, `vm_watch_exit`, `vm_forgetblock`, `vm_forgetblocks`
  - `minix_vfs_mmap`, `minix_mmap_for`, `vm_info_stats`, `vm_info_usage`, `vm_info_region`
  - `vm_procctl_clear`, `vm_procctl_handlemem`, `vm_set_cacheblock`, `vm_map_cacheblock`, `vm_clear_cache`
  - Tests: Unit tests for each type/function; compile-time size/offset assertions where applicable

- [x] **1.10 — Port `minix/dmap.h`**
  - Source: `.refs/minix-3.3.0/minix/include/minix/dmap.h`
  - `dmap.rs`: 67+ major device numbers, 8 memory driver minors, special device IDs
  - `NR_DEVICES` (134), `USB_BASE_MAJOR` (65)
  - `ctrlr(n)` const fn — magic formula mapping controller to IRQ
  - `DEV_RAM` (0x0100), `DEV_IMGRD` (0x0106) — special boot monitor device numbers
  - Memory minors: `RAM_DEV_OLD`, `MEM_DEV`, `KMEM_DEV`, `NULL_DEV`, `BOOT_DEV`, `ZERO_DEV`, `IMGRD_DEV`, `RAM_DEV_FIRST`
  - Tests: Unit tests for each type/function; compile-time size/offset assertions where applicable

- [x] **1.11 — Port `minix/devio.h`**
  - Source: `.refs/minix-3.3.0/minix/include/minix/devio.h`
  - `devio.rs`: `port_t` type, `PvbPair`/`PvwPair`/`PvlPair` structs (packed)
  - Deprecated constants: `MASK_GRANULARITY`, `PVB_FLAG`, `PVW_FLAG`, `PVL_FLAG`
  - Deprecated: `MASK_IN_OR_OUT`, `DEVIO_INPUT`, `DEVIO_OUTPUT`, `PV_BUF_SIZE`
  - Deprecated: `MAX_PVB_PAIRS` (32), `MAX_PVW_PAIRS` (16), `MAX_PVL_PAIRS` (8)
  - Tests: Unit tests for each type/function; compile-time size/offset assertions where applicable

---

## Phase 2: Kernel Low-Level Primitives

**Goal**: Implement the kernel's raw hardware interaction layer before any higher-level logic.

### Tasks

- [x] **2.1 — Arch-specific crate: x86_64 headers**
  - Source: `.refs/minix-3.3.0/sys/arch/i386/include/` (base), `.refs/minix-3.3.0/sys/arch/x86/include/` (common x86)
  - Adapt headers for x86_64 ABI:
  - `param.h` → `param.rs`: Page size (4KB), KERNBASE, conversion macros, paging level constants
  - `vmparam.h` → `vmparam.rs`: VM address space, process size limits, direct mapping constants
  - `segments.h` → `segments.rs`: Segment/gate descriptors, GDT/LDT entries, selector macros
  - `tss.h` → `tss.rs`: 64-bit TSS (256 bytes), RSP0/1/2, IST1-6, MSR base fields
  - `pcb.h` → `pcb.rs`: 64-bit PCB with CR0/CR2/CR3, FPU save area
  - `frame.h` → `frame.rs`: TrapFrame (19 fields), IntrFrame (27 fields), SwitchFrame
  - `mcontext.h` → `mcontext.rs`: Mcontext with 23 GPRs, FPU/XMM state, register indices
  - `multiboot.h` → `multiboot.rs`: Multiboot2 header/info, memory map, modules
  - `psl.h` → `psl.rs`: RFLAGS bits, I/O privilege level helpers
  - `pte.h` → `pte.rs`: PTE format, cacheability bits, PAT indices
  - `pmap.h` → `pmap.rs`: 4-level paging constants, TLB shootdown reasons
  - `cpu_msr.h` → `cpu_msr.rs`: MSR constants, `rdmsr`/`wrmsr` intrinsics
  - `cpuvar.h` → `cpuvar.rs`: CpuInfo struct, CPU roles, attach arguments
  - `apicvar.h`, `pic.h`, `intr.h` → `interrupt.rs`: PIC ports, APIC registers, IRQ mapping
  - All structs use `#[repr(C, packed)]` where C used `__packed`
  - Manual `Default` implementations for arrays >32 elements (Rust limitation)
  - `no_std` crate with `core::mem` and `core::arch::asm!`
  - **124 unit tests** across all modules (functional, edge case, integration)
  - Constants cross-referenced against C headers, struct layouts match `#[repr(C)]`
  - `cpuvar.rs`: CPU role constants fixed to match C reference (SP=0, BP=1, AP=2)
  - `psl.rs`: PSL_CLEARSIG now includes PSL_VM (bit 20) per C reference
  - `cpulocals.rs`: cpu_is_idle/idle_interrupted use AtomicI32 for volatile semantics
  - `cargo clippy --package arch-x86_64 -- -D warnings`: **Clean**

- [x] **2.2 — Port + adapt assembly routines for x86_64**
  - Source: `.refs/minix-3.3.0/minix/kernel/arch/i386/` (i386 reference)
  - Ported into `crates/arch-x86_64/src/asm.rs` using `#[naked]` + `naked_asm!`:
  - `io_inb.S` → `inb`: Read byte from I/O port
  - `io_inw.S` → `inw`: Read word from I/O port
  - `io_inl.S` → `inl`: Read dword from I/O port
  - `io_outb.S` → `outb`: Write byte to I/O port
  - `io_outw.S` → `outw`: Write word to I/O port
  - `io_outl.S` → `outl`: Write dword to I/O port
  - `io_intr.S` → `intr_disable`/`intr_enable`: CLI/STI
  - `debugreg.S` → `st_dr0-7`/`ld_dr0-7`: Debug register access
  - `klib.S` → `phys_copy`: Memory copy with alignment optimization
  - `klib.S` → `phys_insb`/`phys_insw`/`phys_outsb`/`phys_outsw`: I/O port array ops
  - `switch.S` → `switch`: Context switch via `iretq` (saves rbp/rbx/r12-r15, swaps stacks)
  - `cpu_msr.rs` (already exists): `rdmsr`/`wrmsr` MSR access intrinsics
  - `#[unsafe(naked)]` and `#[unsafe(no_mangle)]` for Rust 2024 compatibility
  - **118 tests** across all modules (117 passed, 1 ignored due to sanitizer)
  - `cargo clippy --package arch-x86_64 -- -D warnings`: **Clean**

- [x] **2.3 — Implement raw hardware operations**
  - Created `crates/arch-x86_64/src/hw.rs` (881 lines)
  - **I/O port operations** (re-exported from `asm` module): `inb`, `outb`, `inw`, `outw`, `inl`, `outl`
  - **CR register access**: `read_cr0`, `write_cr0`, `read_cr2`, `read_cr3`, `write_cr3`, `read_cr4`, `write_cr4`
  - **TLB management**: `invlpg`, `tlb_flush_current`, `tlb_flush_global`, `tlb_flush_page`
  - **GDT/IDT/TSS setup**: `lgdt`, `lidt`, `sgdt`, `sidt`, `ltr`, `str`
  - **FPU operations**: `save_fpu`, `restore_fpu`, `fpu_init`
  - **Interrupt gate descriptors**: `idt_gate_descriptor`, `idt_gate_64`, `idt_trap_gate_64`, `idt_int_gate_64`
    - Follows Intel SDM 64-bit gate format (16-byte descriptor layout)
    - `idt_gate_descriptor`: 9-param const fn; returns `u64` packing fields that span 128 bits
      using `wrapping_shl(64)` for `offset_super_high` (bits 64-127 wrap into lower bits)
    - `idt_gate_64`: convenience helper extracting offset_low/selector/offset_mid/offset_super_high
  - **APIC/PIC programming**: `apic_read`, `apic_write`, `pic_read`, `pic_write`, `pic_read_irr`, `pic_read_isr`
  - **Serial port I/O**: `arch_ser_init`, `ser_putc`, `ser_getc`, `ser_puts` (COM1=0x3F8, COM2=0x2F8)
  - **TSC reading**: `read_tsc`, `read_tsc_serialized`, `read_apic_tsc`
  - **Memory barriers/atomic primitives**: `atomic_fence`, `atomic_load_acquire`, `atomic_store_release`,
    `atomic_cas_64/32`, `atomic_exchange_64/32`, `atomic_add_64/32`
  - **CPUID with push/pop rbx workaround**:
    - **Problem**: `cpuid` writes `eax`, `ebx`, `ecx`, `edx`. On Windows x86_64,
      LLVM reserves `rbx` internally. Declaring `out("rbx")` as an asm operand
      produces: *"rbx is used internally by LLVM and cannot be used as an operand
      for inline asm"* -- a compile-time error.
    - This is a *host-compiler constraint*, not a target constraint. The bare-metal
      target (`x86_64-unknown-none`) would accept `out("rbx")` fine, but tests
      run on the Windows host where LLVM rejects it.
    - **Fix**: Don't declare `rbx` as an operand. Route around it:
      ```
      push rbx          ; save rbx (raw text, LLVM doesn't track it)
      mov eax, ecx      ; leaf value into eax
      cpuid             ; writes eax, ebx, ecx, edx
      mov esi, ebx      ; capture ebx into esi (LLVM-allowed output reg)
      pop rbx           ; restore original rbx
      mov edi, edx      ; capture edx into edi (LLVM-allowed output reg)
      ```
      Outputs declared as `out("eax")`, `out("esi")`, `lateout("ecx")`, `out("edi")`.
    - `esi`/`edi` are caller-saved scratch registers on all x86_64 ABIs (SysV,
      Windows x64), so the compiler never depends on their value across the asm.
    - **Why no `#[cfg]`**: The push/pop overhead is negligible (cpuid itself takes
      hundreds of cycles). A single code path avoids maintenance burden with no
      real performance downside.
  - **Clippy suppressions** (module-level `#![allow(...)]`):
    - `missing_safety_doc` — obvious for hardware operations
    - `too_many_arguments` — necessary for flexible gate construction
    - `pointers_in_nomem_asm_block` — asm block writes to pointer
    - `identity_op` — clarity in operations like `outb(port, 3)`
    - `unnecessary_cast` — u64→u64 conversions
  - **`cargo clippy --package arch-x86_64 -- -D warnings`**: **Clean**
  - **`cargo test --package arch-x86_64`**: **180 tests** (179 passed, 1 ignored — physical address pointer sanitizer)

- [x] **2.4 — Implement the raw memory allocator**
  - Created `crates/arch-x86_64/src/alloc.rs` (806 lines)
  - **`PhysicalMemoryMap`**: multiboot memory map management
    - `add()`: add regions with 4 GB truncation
    - `cut()`: remove regions and split subregions back
    - `iter_available()`, `total_available()`, `highest_phys()`
  - **`PhysicalAllocator`**: bitmap-based page allocator
    - `alloc_contig()`: first-fit contiguous allocation
    - `free_contig()`: free with memory map restoration
    - `alloc_page()`: single page from high addresses (like `pg_alloc_page`)
    - `reserve_kernel()`, `reserve_module()`: mark boot regions
    - `free_count()`: count free pages via bitmap
  - Global allocator with `init_allocator()` and `global_allocator()`
  - **15 tests** (all passed): alloc/free, alignment, no-overlap,
    memory map cut/split, 4 GB limit, bitmap operations, exhaustion,
    boundary cuts, double-free, overflow resilience
  - `cargo clippy --package arch-x86_64 -- -D warnings`: **Clean**

- [x] **2.5 — Port `minix/kernel/cpulocals.h`**
  - Created `crates/arch-x86_64/src/cpulocals.rs` (412 lines)
  - **`CpuLocalVars`**: per-CPU local variables mirroring Minix's C struct
    - `proc_ptr`: currently running process
    - `bill_ptr`: process to bill for clock ticks
    - `idle_proc`: idle process stub
    - `pagefault_handled`: recursive pagefault detection
    - `ptproc`: process page tables currently loaded
    - `run_q_head/run_q_tail[NR_SCHED_QUEUES]`: ready list pointers
    - `cpu_is_idle`, `idle_interrupted`: idle state
    - `tsc_ctr_switch`, `cpu_last_tsc`, `cpu_last_idle`: time accounting
    - `fpu_presence`, `fpu_owner`: FPU ownership
  - **`CpuLocalStorage`**: global storage wrapper
  - Global `CPU_LOCAL_STORAGE` static with field offset constants
  - Unsafe accessor functions: `get_*`/`set_*` for each field
  - `NR_SCHED_QUEUES = 16`, `NR_CPUS = MAXCPUS` from param.rs
  - Single-CPU layout (SMP array indexing can be added later)
  - **16 tests** (all passed): defaults, run queue array, idle_proc_ptr,
    storage init/accessors, setters, global init, atomic idle flags
  - `cargo clippy --package arch-x86_64 -- -D warnings`: **Clean**

- [x] **2.6 — Port `minix/kernel/spinlock.h`**
  - Created `crates/arch-x86_64/src/spinlock.rs` (224 lines)
  - **`Spinlock` struct**: wraps `AtomicT` (u32) lock flag
  - **SMP mode**: real spinlock using `hw::atomic_cas_32` (cmpxchg)
    - `spinlock_lock()`: loop until CAS succeeds
    - `spinlock_trylock()`: non-blocking CAS attempt
    - `spinlock_unlock()`: write 0 to unlock
  - **Single-CPU mode**: no-op (CONFIG_SMP = false)
    - `spinlock_init()`, `spinlock_lock()`, `spinlock_unlock()` are no-ops
    - `spinlock_trylock()` always returns true
  - **`Spinlock` struct + macros**: `spinlock_define!`, `private_spinlock_define!`,
    `spinlock_declare!` (mirrors C macro equivalents)
  - **Big Kernel Lock (BKL)**: `BIG_KERNEL_LOCK` static + `bkl_lock()`/`bkl_unlock()`
  - `AtomicT` = `u32` (same as Minix `typedef u32_t atomic_t`)
  - **15 tests** (all passed): init, trylock, unlock, BKL, config,
    macro expansion, double-unlock safe, const construction
  - `cargo clippy --package arch-x86_64 -- -D warnings`: **Clean**

---

## Phase 3: Kernel Core — Process Table & Scheduling

**Goal**: Implement the kernel's process management core — the heart of the microkernel.

### Tasks

- [x] **3.1 — Port `minix/kernel/proc.h` → Rust**
  - Source: `.refs/minix-3.3.0/minix/kernel/proc.h`
  - `struct Proc` ported with all fields (stackframe, segframe, priv, flags, accounting, VM request, etc.)
  - RTS flags: `RTS_SLOT_FREE`, `RTS_PROC_STOP`, `RTS_SENDING`, `RTS_RECEIVING`, `RTS_SIGNALED`, `RTS_SIG_PENDING`, `RTS_P_STOP`, `RTS_NO_PRIV`, `RTS_NO_ENDPOINT`, `RTS_VMINHIBIT`, `RTS_PAGEFAULT`, `RTS_VMREQUEST`, `RTS_VMREQTARGET`, `RTS_PREEMPTED`, `RTS_NO_QUANTUM`, `RTS_BOOTINHIBIT`
  - MF flags: `MF_REPLY_PEND`, `MF_VIRT_TIMER`, `MF_PROF_TIMER`, `MF_KCALL_RESUME`, `MF_DELIVERMSG`, `MF_SIG_DELAY`, `MF_SC_ACTIVE`, `MF_SC_DEFER`, `MF_SC_TRACE`, `MF_FPU_INITIALIZED`, `MF_SENDING_FROM_KERNEL`, `MF_CONTEXT_SET`, `MF_SPROF_SEEN`, `MF_FLUSH_TLB`, `MF_SENDA_VM_MISS`, `MF_STEP`
  - Macros ported as methods: `is_runnable()`, `ptr_ok()`, `is_preempted()`, `no_quantum()`, `used_fpu()`, `kernel_scheduler()`, `proc_nr()`, `set_magic()`
  - Use `bitflags!` for `RtsFlags` and `MiscFlags` types
  - `ProcVmrequest` and `ProcAccounting` sub-structures implemented
  - `StackFrame` (TrapFrame) and `SegFrame` types defined
  - [x] Tests: Size checks on `Proc` (fits within IDLE_PROC_SIZE=1024)
  - [x] Tests: Flag constants have correct bit positions (RTS, MF values)
  - 23 tests: default state, flag set/clear, blocked-on logic, empty/free detection

- [ ] **3.2 — Port `minix/kernel/priv.h` → Rust**
  - Source: `.refs/minix-3.3.0/minix/kernel/priv.h`
  - `struct Priv` ported with all fields (proc_nr, id, flags, async tab, trap mask, IPC map, kcall mask, sig mgr, alarm timer, I/O ranges, memory ranges, IRQs, grants)
  - Privilege flags: `PREEMPTIBLE`, `BILLABLE`, `DYN_PRIV_ID`, `SYS_PROC`, `CHECK_IO_PORT`, `CHECK_IRQ`, `CHECK_MEM`, `ROOT_SYS_PROC`, `VM_SYS_PROC`, `LU_SYS_PROC`, `RST_SYS_PROC`
  - `PrivFlags` bitflags type
  - Global `PRIV_TABLE` and `PRIV_ADDR_PTRS` arrays declared
  - Accessors: `priv_addr()`, `priv_id()`, `id_to_nr()`, `may_send_to()`
  - `IDLE_PRIV` shared idle privilege structure
  - `BEG_PRIV_ADDR`, `END_PRIV_ADDR`, `BEG_STATIC_PRIV_ADDR`, `END_STATIC_PRIV_ADDR`, `BEG_DYN_PRIV_ADDR`, `END_DYN_PRIV_ADDR` constants
  - [ ] Tests: `size_of::<Priv>()` matches C layout
  - [ ] Tests: Field offsets match C layout (`offset_of!` comparisons)

- [ ] **3.3 — Implement process table**
  - Source: `.refs/minix-3.3.0/minix/kernel/table.c`
  - Global `PROC_TABLE` array: `[MaybeUninit<Proc>; NR_PROCS_TOTAL]`
  - `proc_init()` — initializes all process slots with magic numbers, endpoints, and privilege structures
  - `BEG_PROC_ADDR`, `BEG_USER_ADDR`, `END_PROC_ADDR` constants
  - `proc_addr(n)` / `proc_addr_const(n)` — process number to pointer mapping
  - `is_ok_proc_nr()`, `is_empty_proc()`, `is_kernel_nr()`, `is_kernel_proc()`, `is_user_proc()` — validity checks
  - `endpoint_lookup(ep)` — lookup by endpoint
  - `is_ok_endpoint()` — validate endpoint and extract process number
  - `RunQueue` struct with per-priority head/tail arrays
  - [ ] Tests: Allocate all slots; verify none double-assigned
  - [ ] Tests: Free + realloc cycle works correctly

- [ ] **3.4 — Implement scheduling**
  - Source: `.refs/minix-3.3.0/minix/kernel/proc.c`
  - `proc_init()` — process table and privilege table initialization
  - `enqueue()` — add process to run queue tail, handle preemption
  - `dequeue()` — remove process from run queue
  - `enqueue_head()` — insert at front of run queue (for preempted processes)
  - `pick_proc()` — select next runnable process (priority-based, scans 16 queues)
  - `notify_scheduler()` — send quantum-exhausted notification
  - `proc_no_time()` — handle quantum expiry
  - `reset_proc_accounting()` — clear accounting fields
  - `is_idle_proc()` — idle process check
  - `runqueues_ok()` — run queue sanity checker (3-pass validation)
  - Multi-level priority queue (16 levels per `NR_SCHED_QUEUES`)
  - Quantum-based preemptive scheduling
  - [ ] Tests: Priority ordering is correct (higher priority always picks first)
  - [ ] Tests: Quantum expires and triggers reschedule
  - [ ] Tests: Enqueue/dequeue balance (no leak)
  - [ ] Tests: FIFO ordering at same priority
  - [ ] Tests: Enqueue at head works
  - [ ] Tests: Priority mismatch detected by `runqueues_ok()`

- [ ] **3.5 — Implement system.c**
  - Source: `.refs/minix-3.3.0/minix/kernel/system.c`
  - `system_init()` — syscall vector init, IRQ hooks, alarm timers
  - `CALL_VEC` — system call dispatch table (256 entries)
  - `get_priv()` — privilege structure allocation (static or dynamic)
  - `set_sendto_bit()` / `unset_sendto_bit()` / `fill_sendto_mask()` — send capability manipulation
  - `send_sig()` / `cause_sig()` / `sig_delay_done()` — signal delivery (skeleton)
  - `kernel_call()` / `kernel_call_resume()` / `kernel_call_dispatch()` — syscall entry point
  - `sched_proc()` — schedule a process (skeleton)
  - `clear_ipc()` / `clear_endpoint()` / `clear_ipc_refs()` — IPC cleanup (skeletons)
  - `KBILL_KCALL` / `KBILL_IPC` — kernel call billing state
  - [ ] Tests: Privilege escalation/selection works correctly
  - [ ] Tests: Signal delivery reaches target process
  - [ ] Tests: Dynamic privilege allocation
  - [ ] Tests: Privilege ID validation

- [ ] **3.6 — Port `minix/kernel/glo.h` global variables**
  - Source: `.refs/minix-3.3.0/minix/kernel/glo.h`
  - All globals ported: `KINFO`, `MACHINE`, `KMESSAGES`, `LOADINFO`, `KRANDOM`
  - VM globals: `VMREQUEST`, `LOST_TICKS`, `VM_RUNNING`, `CATCH_PAGEFAULTS`, `KERNEL_MAY_ALLOC`
  - IPC globals: `IPC_CALL_NAMES`
  - Interrupt globals: `IRQ_HOOKS`, `IRQ_ACTIDS`, `IRQ_USE`, `SYSTEM_HZ`
  - Misc globals: `BOOTTIME`, `VERBOSEBOOT`, `CPU_HZ[]`, `CPU_INFO[]`
  - Config globals: `CONFIG_NO_APIC`, `CONFIG_APIC_TIMER_X`, `CONFIG_NO_SMP`
  - BKL stats: `KERNEL_TICKS[]`, `BKL_TICKS[]`, `BKL_TRIES[]`, `BKL_SUCC[]`
  - Feature flags: `MINIX_FEATURE_FLAGS`, `MINIX_KERNINFO_USER`
  - Helper types: `volatile_i32`, `IrqHook`, `CpuInfo`, `KInfoStruct`
  - CPU frequency helpers: `cpu_set_freq()` / `cpu_get_freq()`
  - [ ] Tests: Global state is correctly initialized at boot
  - [ ] Tests: CPU frequency helpers work
  - [ ] Tests: No data races under concurrent access (in SMP builds)

- [ ] **3.7 — Port `minix/kernel/debug.c`**
  - Source: `.refs/minix-3.3.0/minix/kernel/debug.c`
  - `runqueues_ok()` — run queue sanity check (returns bool)
  - `rtsflagstr()` / `miscflagstr()` / `privflagstr()` — flag-to-string conversion
  - `schedulerstr()` — scheduler identification
  - `print_proc()` / `print_proc_recursive()` — process table printing (skeletons)
  - `proc_ptr_ok()` — process pointer validation
  - `print_proc_table_summary()` — table summary (skeleton)
  - Debug IPC hooks (skeleton)
  - [ ] Tests: `runqueues_ok()` detects a corrupted queue (via `runqueues_ok` function)
  - [ ] Tests: Full debug IPC hook coverage (proc_ptr_ok, flag strings)

- [ ] **3.8 — Port `minix/kernel/profile.c`**
  - Source: `.refs/minix-3.3.0/minix/kernel/profile.c`
  - Configuration: `SPROFILE`, `CPROFILE`, `SAMPLE_BUFFER_SIZE`, `CPROF_TABLE_SIZE_KERNEL`
  - `init_profile_clock()` / `stop_profile_clock()` — profile clock (skeletons)
  - `sprof_save_sample()` / `sprof_save_proc()` — sample storage (skeletons)
  - `profile_sample()` — statistical sampling
  - `nmi_sprofile_handler()` — NMI profiling handler (skeleton)
  - Call profiling: `CPROF_TBL`, `CPROF_PROCS_NO`, `CPROF_PROC_INFO`
  - `profile_get_tbl_size()` / `profile_get_announce()` / `profile_register()`
  - `SPROFILING`, `SPROF_INFO`, `SPROF_MEM_SIZE` — profiling state
  - [ ] Tests: Profile constants and skeleton functions work
  - [ ] Tests: SprofInfo layout verified

---

**Phase 3 Status**: TODO (0%) — no tasks implemented yet.

## Phase 4: IPC System — Message Passing

**Goal**: Implement the entire IPC subsystem — the backbone of the Minix microkernel architecture.

### Tasks

- [ ] **4.1 — Implement IPC functions from `proc.c`** TODO
  - Source: `.refs/minix-3.3.0/minix/kernel/proc.c`
  - `do_sync_ipc()` — synchronous IPC dispatcher
  - `mini_send()` — blocking send
  - `mini_receive()` — blocking receive
  - `mini_notify()` — asynchronous notify delivery
  - `mini_senda()` — async send entry point
  - `try_deliver_senda()` — async message table delivery
  - `deadlock()` — IPC deadlock cycle detection
  - `has_pending_notify()` / `has_pending_asend()` — pending async message checks
  - `unset_notify_pending()` — clear pending notification
  - `try_async()` / `try_one()` — async delivery helpers
  - `cancel_async()` — cancel pending async
  - `endpoint_lookup()` — endpoint → process pointer lookup
  - `is_ok_endpoint_f()` — endpoint validation
  - `proc_addr()` — get process pointer by slot number
  - `id_to_nr()` — privilege ID to process number conversion
  - `build_notify_message()` — notification message builder
  - `ipc_status_add_call()` / `ipc_status_add_flags()` / `ipc_status_clear()`
  - New `MiscFlags` bits: `FPU_INITIALIZED` (0x2000), `SENDING_FROM_KERNEL` (0x4000)
  - AMF flags: `AMF_VALID`, `AMF_DONE`, `AMF_NOTIFY`, `AMF_NOREPLY`, `AMF_NOTIFY_ERR`
  - Send flags: `NON_BLOCKING`, `FROM_KERNEL`
  - Tests (19 passing):
    - Round-trip send → receive works
    - Deadlock detector catches A→B, B→A cycle
    - Deadlock allows non-fatal SEND+RECEIVE pair
    - Notify delivers without blocking
    - `endpoint_lookup()` returns correct proc pointer for valid endpoints
    - `endpoint_lookup()` returns null for invalid endpoints
    - `is_ok_endpoint_f()` validates correct and rejects out-of-range
    - MiscFlag bit positions verified
    - IPC call numbers match C constants
    - Error codes match C constants
    - AMF flag bitmasks correct
    - Message struct size verified
    - IPC status packing/unpacking roundtrip
    - Endpoint construction roundtrip
    - Pending notification/asend checks

- [ ] **4.2 — Implement message copy infrastructure** TODO
  - `verify_grant()` — validate and resolve grants, following indirect chains
  - `safecopy()` — core safe copy with grant verification and virtual_copy callback
  - `do_safecopy_from()` — SYS_SAFECOPYFROM kernel call
  - `do_safecopy_to()` — SYS_SAFECOPYTO kernel call
  - `do_vsafecopy()` — SYS_VSAFECOPY vectored safe copy
  - Constants: `MAX_INDIRECT_DEPTH`, `MEM_TOP`, `SCPVEC_NR`, `ELOOP`, `EFAULT_SRC`, `EFAULT_DST`
  - Source: `.refs/minix-3.3.0/minix/kernel/system/do_safecopy.c`
  - Tests (28, 26 passing, 2 magic grant tests have a subtle cfg issue):
    - Direct grant: valid copy succeeds
    - Direct grant: offset within range works
    - Direct grant: out-of-range copy fails with EPERM
    - Direct grant: wrong grantee detected
    - Direct grant: ANY grantee matches
    - Direct grant: access permission check (read vs write)
    - Indirect grant: single-hop chain resolves
    - Indirect grant: chain exceeding MAX_INDIRECT_DEPTH returns ELOOP
    - Indirect grant: wrong indirect grantee detected
    - Magic grant: valid grant resolves
    - Magic grant: wrong grantee detected
    - Magic grant: ANY grantee matches
    - Magic grant: wrong_granter detected (production only)
    - Invalid grant ID detected
    - Invalid grant flags detected
    - safecopy with valid grant succeeds
    - safecopy with out-of-range grant fails
    - do_safecopy_from valid copy works
    - do_safecopy_from out-of-range fails
    - do_safecopy_to valid copy works
    - do_safecopy_to out-of-range fails
    - do_vsafecopy single element works
    - do_vsafecopy multiple elements works
    - do_vsafecopy no SELF element fails
    - SCPVEC_NR constant matches C definition
    - Grant flags verified
    - grant_valid() behavior verified
    - MAX_INDIRECT_DEPTH verified

- [ ] **4.3 — Implement address space switching** TODO
  - **Make sure to target x86_64 arch instead of i386**
  - `switch_address_space()` — switch page directory
  - `release_address_space()` — free page tables
  - `switch_address_space_idle()` — special idle case
  - Source: `.refs/minix-3.3.0/minix/kernel/arch/i386/` (arch-specific)
  - Tests: Address space switch changes visible memory

- [ ] **4.4 — In-kernel server dispatch mechanism**
  - `ServerDispatchFn` callback type — routes IPC directly to in-kernel servers
  - `SERVER_DISPATCH` table — indexed by endpoint number (up to 16 entries)
  - `register_server_dispatch()` — register a server handler for an endpoint
  - `try_server_dispatch()` — attempt dispatch before normal process-to-process IPC
  - Integrated into `do_sync_ipc()`: SENDREC/SEND calls check server dispatch first
  - **Exec dispatch handling**: PM_FORK returns 0 (child path), PM_EXEC loads `/bin/sh`
    from initramfs via ELF loader, PM_EXIT returns OK, PM_WAITPID returns ECHILD
  - `SetExecRipFn` callback + `SET_EXEC_RIP` static — arch-specific exec target
  - `set_exec_target()` — set RIP/RSP for syscall return to a new binary
  - Source: `crates/kernel/src/ipc.rs`
  - Tests: Round-trip IPC send/receive cycle; deadlock detection; grant verification; address space switch validation

---

## Phase 5: Kernel System Calls

**Goal**: Implement all ~40 kernel system call handlers.

### Tasks

Implement each `do_*` function in `.refs/minix-3.3.0/minix/kernel/system/`:

- [ ] **5.1 — `do_fork.c`**: `SYS_FORK` — clone process table entry, set up new VM
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.2 — `do_exec.c`**: `SYS_EXEC` — load ELF, set up memory map, switch address space
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.3 — `do_clear.c`**: `SYS_CLEAR` — clean up after process exit
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.4 — `do_exit.c`**: `SYS_EXIT` — process teardown
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.5 — `do_copy.c`**: `SYS_VIRCOPY`, `SYS_PHYSCOPY` — safe memory copy between processes
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.6 — `do_umap.c`**: `SYS_UMAP` — virtual → physical address mapping
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.7 — `do_umap_remote.c`**: `SYS_UMAP_REMOTE` — remote process address mapping
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.8 — `do_vumap.c`**: `SYS_VUMAP` — vectored virtual→physical mapping
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.9 — `do_memset.c`**: `SYS_MEMSET` — write pattern to memory region
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.10 — `do_abort.c`**: `SYS_ABORT` — system shutdown
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.11 — `do_getinfo.c`**: `SYS_GETINFO` — kernel info retrieval
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.12 — `do_privctl.c`**: `SYS_PRIVCTL` — capability management
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.13 — `do_irqctl.c`**: `SYS_IRQCTL` — IRQ policy management
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.14 — `do_devio.c`**: `SYS_DEVIO` — I/O port access
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.15 — `do_vdevio.c`**: `SYS_VDEVIO` — vectored I/O
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.16 — `do_sdevio.c`**: `SYS_SDEVIO` — single I/O request
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.17 — `do_kill.c`**: `SYS_KILL` — send signal
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.18 — `do_getksig.c`**: `SYS_GETKSIG` — get pending kernel signals
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.19 — `do_endksig.c`**: `SYS_ENDKSIG` — end kernel signal handling
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.20 — `do_sigsend.c`**: `SYS_SIGSEND` — send signal with context
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.21 — `do_sigreturn.c`**: `SYS_SIGRETURN` — return from signal
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.22 — `do_times.c`**: `SYS_TIMES` — get timing info
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.23 — `do_setalarm.c`**: `SYS_SETALARM` — set timer alarm
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.24 — `do_vtimer.c`**: `SYS_VTIMER` — virtual timer
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.25 — `do_runctl.c`**: `SYS_RUNCTL` — control process run state
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.26 — `do_statectl.c`**: `SYS_STATECTL` — control process state
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.27 — `do_schedule.c`**: `SYS_SCHEDULE` — schedule a process
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.28 — `do_schedctl.c`**: `SYS_SCHEDCTL` — scheduling control
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.29 — `do_setgrant.c`**: `SYS_SETGRANT` — set grant table
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.30 — `do_trace.c`**: `SYS_TRACE` — kernel tracing
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.31 — `do_safecopy.c`**: `SYS_SAFECOPYFROM`, `SYS_SAFECOPYTO`, `SYS_VSAFECOPY` — grant-based safe copy
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.32 — `do_safememset.c`**: `SYS_SAFEMEMSET` — grant-based memset
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.33 — `do_vmctl.c`**: `SYS_VMCTL` — VM control
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.34 — `do_settime.c`, `do_stime.c`**: `SYS_SETTIME`, `SYS_STIME` — time of day
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.35 — `do_mcontext.c`**: `SYS_GETMCONTEXT`, `SYS_SETMCONTEXT` — machine context
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.36 — `do_diagctl.c`**: `SYS_DIAGCTL` — diagnostic control
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.37 — `do_cprofile.c`, `do_profbuf.c`**: `SYS_CPROF`, `SYS_PROFBUF` — call profiling
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.38 — `do_update.c`**: `SYS_UPDATE` — live update support
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall
- [ ] **5.39 — `do_vtimer.c`**: `SYS_VTIMER` — virtual timer check
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall

- [ ] **5.40 — IPC syscall handlers (kernel syscall numbers 46–49)**
  - `ipc_send_handler` (46) — routes to `kernel::ipc::do_sync_ipc()` with SEND
  - `ipc_receive_handler` (47) — routes to `kernel::ipc::do_sync_ipc()` with RECEIVE
  - `ipc_sendrec_handler` (48) — routes to `kernel::ipc::do_sync_ipc()` with SENDREC
  - `ipc_notify_handler` (49) — routes to `kernel::ipc::do_sync_ipc()` with NOTIFY
  - `CURRENT_PROC` tracking via `set_current_proc()` / `current_proc()`
  - `register_ipc_syscalls()` — registers all four handlers in the syscall table
  - `SYS_MAX` increased from 45 to 50
  - Source: `crates/arch-x86_64/src/arch_syscall.rs`
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall

- [ ] **5.41 — Basic userspace-facing syscall handlers**
  - `sys_getpid_handler` (0) — returns PID 1 (init)
  - `sys_exit_handler` (2) — halts CPU (no process cleanup yet)
  - `sys_write_handler` (9) — writes buffer to serial (fd 1=stdout, 2=stderr)
  - `sys_brk_handler` (13) — simple bump allocator in 0x3FE00000–0x3FF00000 region
  - All registered in kmain before switching to userspace
  - Source: `crates/kernel-boot/src/main.rs`

> Each system call task: Test with a userspace program that issues the syscall and verifies the result.

### Implementation notes

**Group 1 (tasks 5.1–5.4): Stub handlers registered in `system_init()`.**

`do_exit` has a minimal working body (causes SIGABRT, returns EDONTREPLY).
The others (`do_fork`, `do_exec`, `do_clear`) are full skeleton stubs —
they return a constant and have detailed doc comments mapping each C line
to its Rust counterpart. Full bodies wait for VM server and IPC msg access.

**Group 2 (tasks 5.5–5.9): `todo!()` stubs registered in `system_init()`.**

These use `todo!()` so they panic at runtime — impossible to miss during
debugging. Each `todo!()` message explains the missing dependency:

- `do_copy` — needs `virtual_copy` / `virtual_copy_vmcheck` from vm module
- `do_umap` — delegates to `do_umap_remote`
- `do_umap_remote` — needs `vm_lookup`, `vm_lookup_range`, `verify_grant`
- `do_vumap` — needs vector processing + `vm_lookup_range` + `verify_grant`
- `do_memset` — needs `vm_memset` from vm module

All 5 are registered in `CALL_VEC` via `map_syscall()`, so dispatch works
and only the runtime call path fails when invoked.

**Group 3 (tasks 5.10–5.11): Stub handlers registered in `system_init()`.**

- `do_abort` — calls `prepare_shutdown(how)`, returns OK
- `do_getinfo` — large switch with ~20 request types (GET_MACHINE, GET_KINFO,
  GET_PROCTAB, GET_PROC, GET_PRIV, GET_REGS, GET_WHOAMI, GET_RUSAGE,
  GET_RANDOMNESS, etc.), each setting src_vir and length for data_copy

**Group 4 (tasks 5.12–5.14, 5.17):**

- `do_privctl` — stub with `todo!()`, needs data_copy + 10+ privilege handlers
- `do_irqctl` — stub with `todo!()`, needs irq_hooks + put_irq_handler
- `do_devio` — stub with `todo!()`, needs priv() macro + inb/outb etc.
- `do_kill` — **REAL implementation** (not a stub). Validates endpoint,
  signal range, rejects kernel targets, calls cause_sig. Includes 3 tests:
  `test_do_kill_invalid_endpoint`, `test_do_kill_signal_number_bounds`,
  `test_do_kill_kernel_target_rejected`

**Group 5 (tasks 5.15–5.16, 5.18–5.21): `todo!()` stubs registered in `system_init()`.**

- `do_sdevio` — single device I/O, needs `priv()` + CHECK_IO_PORT + inb/outb
- `do_vdevio` — vectored device I/O, same deps + `data_copy` + loop over entries
- `do_getksig` — signal manager query, needs proc table iteration + sig_mgr check
- `do_endksig` — end kernel signal, needs sig_mgr check + RTS_SIG_PENDING
- `do_sigsend` — POSIX signal send, needs `data_copy_vmcheck` + sigframe setup
- `do_sigreturn` — signal return, needs `arch_proc_setcontext` + sigcontext restore

**Group 6 (tasks 5.22–5.28): `todo!()` stubs registered in `system_init()`.**

- `do_times` — timing info, needs proc accounting fields + monotonic/realtime
- `do_setalarm` — alarm timer, needs `priv()` + s_alarm_timer + timer APIs
- `do_vtimer` — virtual timer, needs MF_VIRT/MF_PROF flags + tick-left fields
- `do_runctl` — process stop/resume, needs RTS_PROC_STOP + RC_DELAY logic
- `do_statectl` — state control, needs `clear_ipc_refs` dispatch
- `do_schedule` — process scheduling, needs RTS_NEEDS_SCHEDULE + enqueue
- `do_schedctl` — scheduling control, needs SCHEDCTL_FLAG_KERNEL + params

**Group 7 (tasks 5.29–5.32): `todo!()` stubs registered in `system_init()`.**

- `do_setgrant` — grant table setup, needs `priv()` + _K_SET_GRANT_TABLE
- `do_trace` — ptrace (15+ commands), needs vmcheck + ptrace dispatch
- `do_safecopy_from` — safe copy from, needs verify_grant + virtual_copy
- `do_safecopy_to` — safe copy to, needs verify_grant + virtual_copy
- `do_vsafecopy` — vectored safe copy, needs vector loop + safecopy

**Group 8 (tasks 5.33–5.39): `todo!()` stubs registered in `system_init()`.**

- `do_vmctl` — VM control, needs VM parameter dispatch + arch_phys_map
- `do_settime` / `do_stime` — time of day, needs clock time update
- `do_getmcontext` / `do_setmcontext` — machine context, needs proc_addr + copy
- `do_diagctl` — diagnostic control, needs DIAGCTL_CODE dispatch + buffer
- `do_cprofile` / `do_profbuf` — call profiling, needs profile buffer control
- `do_update` — live update, needs update handshake
- `do_safememset` — grant-based memset, needs verify_grant + vm_memset

All remaining Phase 5 syscalls (5.15–5.39) are registered in `CALL_VEC`
via `map_syscall()` and use `todo!()` stubs with detailed documentation
of the C-line-by-line porting logic. Each stub clearly states its
dependencies so future implementers know what's needed.
  - Tests: Unit test for the syscall handler; verify return codes; test with userspace program that issues the syscall

---

## Phase 6: Virtual Memory System

**Goal**: Implement the VM server (`.refs/minix-3.3.0/minix/servers/vm/`) — the process that manages physical memory and page tables.

### Tasks

- [ ] **6.1 — Implement physical memory manager**
  - Memory hole detection (from multiboot info in `.refs/minix-3.3.0/minix/kernel/main.c` `kmain()`)
  - Free list / buddy allocator
  - Page frame allocation/deallocation
  - Physical memory map (`phys_map` array)
  - Tests: Allocate all available pages; free them all

- [ ] **6.2 — Implement page table management**
  - Page directory allocation
  - Page table entry setup (PDE/PTE)
  - Page fault handling
  - TLB invalidation
  - `enable_paging()` — activate paging for a process
  - Tests: Map/unmap page; verify access faults correctly

- [ ] **6.3 — Port `vm_main.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/main.c`
  - VM server main loop with SEF init callbacks
  - Message dispatch for VM_PAGEFAULT, RS_INIT, VFS transactions
  - `exec_bootproc()` stub — execute boot processes with ELF loading
  - `do_procctl_notrans()` wrapper for procctl without VFS transid
  - `sef_signal_handler()` callback for kernel signals
  - Call dispatch table (`init_call_table`) with stub handlers for all
    VM calls (VM_MMAP, VM_MUNMAP, VM_EXIT, VM_FORK, VM_BRK, etc.)
  - Dispatched to: `do_mmap`, `do_munmap`, `do_map_phys`, `do_exit`,
    `do_fork`, `do_brk`, `do_willexit`, `do_notify_sig`, `do_procctl`,
    `do_vfs_reply`, `do_vfs_mmap`, `do_rs_set_priv`, `do_rs_update`,
    `do_rs_memctl`, `do_remap`, `do_get_phys`, `do_get_refcount`,
    `do_info`, `do_query_exit`, `do_watch_exit`, `do_mapcache`,
    `do_setcache`, `do_clearcache`, `do_getrusage`
  - Tests: `cargo build` clean, `cargo test --package servers` (16 passed)

- [ ] **6.4 — Port `vm_kern.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/vm_kern.c` (not in Minix 3.3.0 tree)
  - Kernel-side VM operations in `crates/kernel/src/vm.rs`:
    - `KERN_PHYS_MAP` — kernel physical mapping table (16 entries, zeroed static)
    - `KernPhysMapEntry` — kpme_physaddr, kpme_virtaddr, kpme_len
    - `kern_map()`: iterates KERN_PHYS_MAP for free entry (physaddr==0 && virtaddr==0),
      sets entry fields, returns 0 on success or -1 if table full
    - `kern_unmap()`: finds entry by virtaddr, verifies length matches,
      clears all fields, returns 0 on success or -1 if not found
    - `phys_map_add()`: delegates to kern_map() for consistency
    - `phys_map_remove()`: finds entry by physaddr, clears all fields,
      returns 0 on success or -1 if not found
  - Tests: Unit/integration tests for each VM operation;  passes

- [ ] **6.5 — Port `vm_proc.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/vm_proc.c` (not in Minix 3.3.0 tree)
  - Per-process VM operations added to `crates/servers/src/vm/proc.rs`:
    - `pt_new()` — allocate new page directory stub
    - `pt_bind()` — bind page table to Vmproc stub
    - `vm_create()` — initialize new Vmproc for boot process stub
    - `vm_destroy()` — release process address space stub
    - `vm_clone()` — clone address space for fork stub
  - Tests: `cargo build` clean, `cargo test --package servers` passes

- [ ] **6.6 — Port `vm_copy.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/vm_copy.c` (not in Minix 3.3.0 tree)
  - Memory copy operations added to `crates/servers/src/vm/proc.rs`:
    - `vm_copy()` — cross-address-space memory copy with VM checks stub
    - `vm_copy_overwrite()` — overlap-aware memory overwrite stub
    - `vm_collect()` — iterate regions and collect physical pages stub
  - Tests: `cargo build` clean, `cargo test --package servers` passes

- [ ] **6.7 — Port `vm_mem.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/vm_mem.c` (not in Minix 3.3.0 tree)
  - Memory grant management added to `crates/servers/src/mem.rs`:
    - `Grant` struct: g_grantor, g_endpoint, g_vaddr, g_grant_type, g_physaddr, g_npages
    - `GRANT_TABLES` — global grant table [[Grant; 16]; 64]
    - `sys_vm_map()`: validates endpoints, finds free slot via find_free_grant(), computes pages, calls map_grant(), builds & stores Grant entry
    - `sys_vmctl()`: dispatches VMCTL commands (GET_PDBR, MEMREQ_GET/REPLY, NOPAGEZERO, KERNELLIMIT, FLUSHTLB, VMINHIBIT_SET/CLR, CLEARMAPCACHE, BOOTINHIBIT_CLEAR)
    - `find_free_grant()`: walks GRANT_TABLES[ep] for g_grantor==0
    - `map_grant()`: validates endpoint/pages, for GRANT_PHYS returns physaddr, otherwise finds suitable vaddr
    - `grant_physmem()`: validates endpoints, finds slot, calls map_grant(), stores grant
    - `grant_alloc()`: validates page-aligned physaddr, reasonable page count
    - `grant_free()`: walks all GRANT_TABLES, finds matching physaddr+npages, clears all fields
  - Tests: `cargo build` clean, `cargo test --package servers` passes

- [ ] **6.8 — Port `vm_info.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/vm_info.c`
  - VM info queries
  - `do_info()` handler in `crates/servers/src/vm/call.rs` — dispatches `VMIW_STATS`, `VMIW_USAGE`, `VMIW_REGION` queries
    - `VMIW_STATS`: populates `VmStatsInfo` with page size, total pages, free/cached stats
    - `VMIW_USAGE`: populates `VmUsageInfo` from the target process's `Vmproc` entry
    - `VMIW_REGION`: walks region array, writes `VmRegionInfo` structs to output buffer
  - Added `PROT_READ`, `PROT_WRITE`, `PROT_EXEC`, `MAP_SHARED`, `MAP_PRIVATE` constants to `kernel::vm`
  - All fields access union safely with explicit `unsafe` blocks
  - Tests: `cargo check --package servers` clean

- [ ] **6.9 — Port `pagefaults.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/pagefaults.c`
  - Page fault handling
  - `do_pagefaults()` in `crates/servers/src/vm/mod.rs` — validates endpoint, checks address against vm_region_top, increments major/minor fault counters, sends SIGSEGV on invalid address
  - `sys_kill()` — validates endpoint/signal, sets SIG_PENDING+SIGNALED flags, enqueues process via `kernel::sched::schedule::enqueue_head`
  - `clear_pagefault()` — clears RTS_PAGEFAULT flag via `kernel::sched::table::proc_addr`
  - `pferr_*()` helpers in `crates/kernel/src/com.rs` — PFERR_NOPAGE, PFERR_WRITE, PFERR_PROT, PFERR_READ
  - `do_vmctl()` in `crates/kernel/src/sched/system.rs` — handles VMCTL_CLEAR_PAGEFAULT by clearing RTS_PAGEFAULT
  - `do_info()` VMIW_STATS — now reads vsi_free/largest from `physmem::allocator()`
  - Added SIGSEGV, SIGILL, SIGABRT signal constants
  - Tests: `cargo test --package servers` 16/16 pass

- [ ] **6.10 — Port `vm_shm.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/vm_shm.c`
  - Shared memory support
  - Tests: Unit/integration tests for each VM operation;  passes

- [ ] **6.11 — Port `vm_remap.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/mmap.c` (remap functions live in mmap.c)
  - `do_remap()` — maps a region from one process to another (VM_REMAP/VM_REMAP_RO)
    - Validates endpoints, source address/size
    - Rounds size to page boundary
    - Creates shared region with VR_SHARED flag
    - Sets protection based on readonly parameter
  - `do_map_phys()` — maps physical memory into process address space
    - Validates length, target endpoint, SELF resolution
    - Rounds addresses to page boundaries
    - Allocates region entries in stubbed region array
    - Returns mapped address with offset
  - `do_get_phys()` — gets physical address for virtual address
    - Validates endpoint
    - Walks region array to find matching region containing addr
    - Returns address (stubbed, full impl needs region tree)
  - `do_get_refcount()` — gets reference count for a region
    - Validates endpoint
    - Walks region array to find matching region
    - Returns reference count (stubbed, returns 1 for matched)
  - `do_munmap()` — unmaps memory regions (VM_UNMAP_PHYS, VM_SHM_UNMAP, regular)
    - Validates endpoint, checks page alignment
    - Walks region array to clear matching entries
  - All functions use stubbed region array (real impl needs `map_page_region`, `map_lookup`, `map_unmap_range`)
  - `cargo test --package servers` 39 passed
  - `cargo check --package servers` clean

- [ ] **6.12 — Port `vm_procctl.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vm/exit.c::do_procctl()`
  - `do_procctl()` — dispatches VM_PROCCTL messages to process-level VM operations
    - `VMPPARAM_CLEAR` — validates source is RS or VFS, calls `clear_proc()`, `pt_new()`, `pt_bind()`
    - `VMPPARAM_HANDLEMEM` — validates source is VFS, reads memory handling params, stubbed (full impl needs `handle_memory_start()` with VFS IPC)
    - Unknown params return EINVAL
  - Endpoint validation uses `NR_PROCS` (256) for bounds checking
  - Message field access uses `m.m_payload.m_m9` union (VMPCTL_WHO=m9l2, VMPCTL_PARAM=m9l1, VMPCTL_M1=m9l3, VMPCTL_LEN=m9l4, VMPCTL_FLAGS=m9l5)
  - Fixed `pt_new` stub to return 0 (success) and zero vm_pt
  - 8 new tests for procctl (CLEAR from RS/VFS/other, HANDLEMEM from VFS/RS, invalid endpoint, unknown param, negative endpoint)
  - `cargo test --package servers` 47 passed
  - `cargo check --package servers` clean

---

## Phase 6.5 — Per-Process Page Tables

**Goal**: Give each process its own address space with private physical copies of code
and stack pages, preventing one process from reading or writing another's memory.
This spans VM (page table construction), arch-x86_64 (syscall entry/exit CR3 save/restore),
and IPC (message delivery under target's CR3).

> **Reference**: See `PER_PROC_PAGE_TABLES.md` for full implementation details,
> assembly snippets, and architectural rationale.

### Tasks

- [ ] **6.5.1 — Save/restore per-process CR3 on every syscall entry/exit**
  - In `syscall_entry` (arch-x86_64): save incoming CR3 on stack BEFORE loading BOOT_CR3
  - On normal return path: restore saved CR3 AFTER handler completes, before `swapgs`+`sysretq`
  - Stack layout (top to bottom): saved CR3 | saved rcx (RIP) | saved r11 (RFLAGS) | saved user RSP
  - The saved CR3 shifts all subsequent stack offsets by +8 (FORK_PARENT_RSP must use `[rsp+24]` not `[rsp+16]`)
  - Init always enters with BOOT_CR3, so save/restore is a no-op (identity value)
  - Source: `crates/arch-x86_64/src/syscall.S`, `crates/arch-x86_64/src/arch_syscall.rs`
  - Reference implementation: `mov rax, cr3; push rax` at entry; `mov rax, [rsp+0]; mov cr3, rax` on return
  - Tests: Syscall round-trip preserves per-process CR3; FORK offsets correct after +8 shift

- [ ] **6.5.2 — exec_setup_new_page_table: create per-process page table at exec time**
  - Called from `asm_exec_handler` in `arch_syscall.rs` after `load_elf` and `setup_user_stack`
  - Allocates PML4, PDP, PD (zeroed 4KB pages via `alloc_phys_page`)
  - Deep-copies all 512 BOOT_PD entries (kernel identity map for 0–1GB) into new PD
  - Links PML4[0] → new PDP → new PD for the private identity map
  - Shares PML4[L4_SLOT_DIRECT] → BOOT_PDP (kernel high mapping)
  - Shares PDP[3] → BOOT_PD3 (shared APIC MMIO mapping)
  - Splits 2MB PDEs at code and stack virtual ranges into 4KB page tables
  - Allocates new physical frames for each 4KB page in those ranges
  - Copies data from identity-mapped originals to private frames via `core::ptr::copy_nonoverlapping`
  - Remaps each virtual page to its private frame in the new page table
  - Writes `p_cr3` and `p_cr3_v` on the exec'd process's `Proc` struct
  - Source: `crates/arch-x86_64/src/arch_syscall.rs`, `crates/arch-x86_64/src/syscall.S`
  - Tests: Exec'd process runs in private address space; private frames differ from identity originals; APIC MMIO still accessible

- [ ] **6.5.3 — Exec target CR3 restore in syscall return path**
  - After `syscall_exec_check` returns non-zero (exec target), load `CURRENT_PROC → p_cr3`
    from Proc struct (offset 0x80) and switch CR3
  - CR3 switch happens BEFORE switching RSP to user space (kernel stack must remain accessible)
  - If `p_cr3` is zero (no per-process page table), skip the switch
  - Source: `crates/arch-x86_64/src/syscall.S`
  - Reference: `lea rax, [rip + CURRENT_PROC]; mov rax, [rax]; test rax, rax; jz ...; mov rax, [rax + 0x80]; mov cr3, rax`
  - Tests: Exec target return switches CR3; zero p_cr3 skips switch; kernel stack accessible after CR3 switch

- [ ] **6.5.4 — delivermsg: write IPC messages under target's per-process CR3**
  - In `delivermsg()` (kernel IPC), temporarily switch to the target process's CR3
    before writing IPC messages to its user buffer
  - Switch back to BOOT_CR3 after the write completes
  - If `p_cr3` is zero (process has no per-process page table, e.g. init), skip CR3 switch
    entirely — write goes through BOOT_CR3, correct for identity-mapped processes
  - Source: `crates/kernel/src/ipc.rs`
  - Tests: IPC message lands in target's private frames (not identity originals);
    init receives messages correctly via BOOT_CR3; zero-p_cr3 path works

- [ ] **6.5.5 — Fork: create child page table with private copies of parent's pages**
  - Implement `pt_new_for_fork()` in `servers/src/vm/proc.rs`:
    - Get parent's CR3 via `vm_get_addrspace(parent_ep)`
    - Walk parent's page table (PML4[0] → PDP[0] → PD, covers 0–1GB)
    - For each PDE with PG_U set:
      - If still a 2MB huge page (PG_PS): child keeps shared identity mapping
      - If split into 4KB page table: walk all 512 PTEs
        - For each PTE with PG_U + PG_P: private-copy frame to child
    - Allocate new physical frames, copy data from parent's frames through identity map
    - Split child's PDE and remap each page to its private frame
    - Bind child's page table via `pt_bind()`
  - Update `do_fork()` in `servers/src/vm/call.rs`:
    `proc::pt_new(child); proc::pt_new_for_fork(child, parent_ep); proc::pt_bind(child);`
  - Add `vm_get_addrspace(ep)` helper in `crates/kernel/src/vm.rs` — returns physical
    address of a process's PML4, or 0 if none
  - Source: `servers/src/vm/proc.rs`, `servers/src/vm/call.rs`, `kernel/src/vm.rs`
  - Tests: Child page table is independent of parent; parent writes are invisible to child;
    shared 2MB huge pages remain shared; zero-p_cr3 handled correctly

- [ ] **6.5.6 — Map kernel BSS with NX in per-process page tables**
  - Enable EFER_NXE bit during boot (in `cstart.rs::enable_long_mode()`) so the NX
    execute-disable bit is active on x86_64
  - Define `PG_NX` constant in `arch-x86_64/src/pte.rs` (alias for `PG_I`, bit 63)
  - Implement `pt_mapkernel()` in `servers/src/vm/pagetable.rs`:
    - Split the 2MB PDE covering kernel text/data/BSS (at 0x200000) into 4KB pages
    - Set `PG_NX` on BSS pages (from `__bss_start` to `__bss_end`), preventing
      accidental code execution from kernel data pages
    - Clear global bit (PG_G) on these entries so TLB invalidates correctly on CR3 switch
  - Source: `crates/arch-x86_64/src/pte.rs`, `servers/src/vm/pagetable.rs`,
    `crates/kernel-boot/src/cstart.rs`
  - Tests: BSS pages have NX set; kernel text pages do NOT have NX; global bit cleared on BSS entries;
    EFER_NXE is enabled at boot

- [ ] **6.5.7 — Regression checks for per-process page tables**
  - IPC dispatch with per-process CR3: `asm_exec_handler` reads `caller.p_reg` fields.
    Proc struct (kernel BSS, identity-mapped) is always accessible. Message buffer (`m_ptr`)
    is user-space, accessible through per-process CR3. Verify both paths work.
  - Timer interrupt during per-process CR3: timer handler calls `apic::eoi()`. With
    BOOT_PD3 shared via PDP[3], per-process page table must map APIC MMIO. Verify.
  - `delivermsg_check()` with per-process CR3: writes to target process's message buffer
    through BOOT_CR3. May not be visible if target runs on per-process CR3. Verify
    the Phase 6.5.4 fix handles this.
  - fork() with per-process CR3: child needs its own page table. Phase 6.5.5 handles this.
  - Write handler reads user data through BOOT_CR3 identity map. For exec'd processes,
    identity-mapped originals match private copies (load_elf wrote them). For forked
    processes, identity-mapped originals may differ from private copies. This is a known
    limitation documented in the current implementation.
  - Tests: Each regression check has a corresponding integration test

### Key Architecture Decisions

1. **`load_elf` writes through BOOT_CR3 (identity map)**: ELF segment writes go to physical
   addresses matching their virtual addresses. The private per-process page table is
   constructed AFTER load_elf, using the identity-mapped data as source material.

2. **Per-process page tables constructed after load_elf**: (a) Create fresh PML4→PDP→PD,
   (b) Deep-copy boot PD identity entries, (c) Split 2MB PDEs at relevant ranges,
   (d) Allocate new frames and copy identity data, (e) Remap virtual pages to private
   frames, (f) Write p_cr3 on Proc struct.

3. **CR3 restored before user RSP switch**: The kernel stack must remain accessible if
   an interrupt fires after CR3 switch but before the return completes.

4. **Init never needs per-process tables**: Init runs on BOOT_CR3. Its saved/restored CR3
   is BOOT_CR3 (a no-op). The delivermsg zero-p_cr3 skip handles this for IPC.

### Current Kernel Page Permissions in Per-Process Page Tables

| Range | Type | Permissions |
|-------|------|-------------|
| 0x000000–0x1FFFFF | User identity | RWX (unchanged) |
| 0x200000–kernel_start | Kernel text | Split to 4KB, read-only, exec (no PG_NX) |
| kernel_start–__bss_start | Kernel text/rodata/data | Split to 4KB, readable/writable, exec |
| __bss_start–__bss_end | Kernel BSS | Split to 4KB, readable/writable, NX |
| 0x400000–user_top | User identity | RWX (unchanged) |
| KERNBASE+offset | Kernel high map | 2MB pages, RW (shared BOOT_PDP) |
| PDP[3] | APIC MMIO | RW (shared BOOT_PD3) |

---

## Phase 7: Clock, Interrupts & Timer

**Goal**: Implement the clock task and kernel interrupt handling.

### Tasks

- [ ] **7.1 — Port `minix/kernel/clock.c`**
  - Source: `.refs/minix-3.3.0/minix/kernel/clock.c`
  - `get_realtime()` / `set_realtime()`, `get_monotonic()`, `set_kernel_timer()`, `cycles_accounting_init()`, `context_stop()` / `context_stop_idle()`
  - Tests: Timer fires at correct interval
  - Implementation: `crates/kernel/src/clock.rs` (371 lines)
  - Timer queue: `MinixTimer` struct, `TimerArg` union, `tmrs_settimer`/`tmrs_clrtimer`/`tmrs_exptimers`
  - Clock accessors: `get_monotonic`, `set_monotonic`, `get_realtime`, `set_realtime`, `tick`
  - Time conversion: `ms_2_cpu_time`, `cpu_time_2_ms`, `set_system_hz`
  - Adjtime support: `set_adjtime_delta`, `get_adjtime_delta`
  - Compile-time size verification for `MinixTimer` (32 bytes) and `TimerArg` (8 bytes)

- [ ] **7.2 — Port `minix/kernel/interrupt.c`**
  - Source: `.refs/minix-3.3.0/minix/kernel/interrupt.c`
  - `put_irq_handler()`, `rm_irq_handler()`, `enable_irq()`, `disable_irq()`, `intr_init()`
  - Tests: IRQ handler registration and firing
  - Implementation: `crates/kernel/src/interrupt.rs` (435 lines)
  - `IrqHook` struct with sorted linked list per IRQ
  - `put_irq_handler`: Register handler with bitmap ID assignment
  - `rm_irq_handler`: Remove handler from list (fixed pointer traversal bug)
  - `irq_handle`: Dispatch to all handlers with active bit management
  - `enable_irq` / `disable_irq`: Hardware mask management
  - Hardware stubs: `hw_intr_used`, `hw_intr_not_used`, `hw_intr_mask`, `hw_intr_unmask`, `hw_intr_ack`
  - 7 unit tests in kernel + integration tests in servers

- [ ] **7.3 — Port `minix/kernel/smp.c`**
  - Source: `.refs/minix-3.3.0/minix/kernel/smp.c`
  - SMP boot, IPI handling, per-CPU lock management
  - Implementation: `crates/kernel/src/smp.rs` (365 lines)
  - CPU state: `NCPUS`, `BSP_CPU_ID`, `CpuInfo` with flag management
  - IPI infrastructure: `SchedIpiData`, handler registration, invocation
  - Big Kernel Lock: `bkl_lock`, `bkl_unlock`, `bkl_is_held`
  - AP boot management: `wait_for_aps_to_finish_booting`, `ap_boot_finished`
  - CPU frequency tracking: `cpu_set_freq`, `cpu_get_freq`
  - 8 unit tests + integration test coverage

- [ ] **7.4 — Port `minix/servers/clock/` clock task** (partial)
  - Source: `.refs/minix-3.3.0/minix/servers/clock/` (all `.c` files)
  - Clock task main loop, timer interrupt handling, alarm delivery
  - Implementation: `crates/servers/src/clock_server.rs` (295 lines)
  - `ClockTimeSpec` type for timespec conversion
  - `ClockId` enum (Realtime/Monotonic)
  - Time resolution queries, alarm timer management
  - 11 tests covering resolution, time specs, tick advancement, adjtime

- [ ] **7.5 — Port `minix/servers/pm/` Power Manager** (types + infra)
  - Source: `.refs/minix-3.3.0/minix/servers/pm/` (all `.c` files)
  - Power management protocol, ACPI integration
  - Implementation: `crates/servers/src/pm.rs` (543 lines)
  - `SigSet` for signal masks, `Itimerval`/`TimeVal` for interval timers
  - `MProc` process manager slot with compile-time offset verification
  - Process table: `MPROC` array, `alloc_proc`, `free_proc`, `init_proc`
  - Alarm management: `set_alarm`, `alarm_is_active`
  - Bug fix: `free_proc` now correctly decrements `PROCS_IN_USE`
  - 9 tests covering sigset ops, process allocation, alarm lifecycle

---

## Phase 7.6 — APIC / I/O APIC Initialization

**Goal**: Initialize the Local APIC and I/O APIC to properly route hardware
interrupts. On x86_64, the APIC is always present and enabled, but its default
configuration (set by QEMU/SeaBIOS) can deliver interrupt sources (like the PIT)
as **NMIs that bypass IF**. This causes timer interrupts to fire even when the
kernel has disabled interrupts (e.g., during syscall handling). The 8259 PIC is
_not_ being used — its ISR reads back 0x00.

### Background / Problem

- On x86_64 in QEMU, the Local APIC is enabled by default after RESET.
- SeaBIOS or QEMU's default config may route IRQ 0 (PIT) to LINT0 of the Local
  APIC in **NMI delivery mode** (delivery mode bits 8-10 = 101b).
- NMI delivery ignores IF, so `cli` and IA32_FMASK cannot block it.
- The 8259 PIC remap and mask are ineffective because interrupts don't go through
  the PIC at all.
- The current boot sequence only initializes the legacy 8259 PIC, leaving the
  APIC in its default (unsafe) state.

### Tasks

- [ ] **7.6.1 — Add APIC base address detection**
  - Read IA32_APIC_BASE MSR (0x1B) to get the physical base address of the
    Local APIC (typically 0xFEE00000).
  - Extract APIC global enable (bit 11) and BSP flag (bit 8).
  - Map the APIC base (identity-mapped; 0xFEE00000 is in the 3-4GB range
    covered by PD3 page table).
  - Tests: MSR read returns a valid address, BSP flag is set.

- [ ] **7.6.2 — Read Local APIC version and LVT entries**
  - Read APIC Version Register (offset 0x30): version + max LVT entry count.
  - Read LVT LINT0 Register (offset 0x350, or 0xF350 for x2APIC): check
    delivery mode field (bits 8-10).  If mode = NMI (101b), the PIT is
    delivered as NMI.
  - Read LVT LINT1 Register (offset 0x360) and LVT Error (offset 0x370).
  - Tests: Version register is readable, LINT0 delivery mode is identified.

- [ ] **7.6.3 — Reprogram LVT LINT0 for Fixed delivery**
  - If LVT LINT0 is NMI or ExtINT, reprogram to:
    - Delivery Mode = Fixed (000b)
    - Delivery Status = Idle (bit 12 = 0)
    - Polarity = Active high (bit 13 = 0)
    - Trigger Mode = Edge (bit 15 = 0)
    - Mask = 1 (bit 16 = 1) — kept masked; interrupt system unmasks later
    - Vector = 0 (unused when masked)
  - This prevents LINT0 from generating NMIs.

- [ ] **7.6.4 — Set up Spurious Interrupt Vector**
  - Write SVR (offset 0xF0/0x0F0):
    - Bit 8 = 1 (APIC software enable)
    - Bits 0-7 = spurious vector (typically 0xFF)
  - Tests: SVR readback matches written value.

- [ ] **7.6.5 — Initialize I/O APIC (mask all RTEs)**
  - Read I/O APIC base from MP table / ACPI MADT, or probe standard address
    0xFEC00000.
  - Read IOAPICVER (index 0x01) to get max RTE entry index.
  - Write all RTEs (0..max) with bit 16 = 1 (masked).
  - Tests: Version register matches expected, all RTEs are masked.

- [ ] **7.6.6 — Wire PIT interrupt through I/O APIC to vector 32**
  - Configure RTE for IRQ 0 (PIT):
    - Vector = 32, Delivery Mode = Fixed, Physical destination
    - Edge-triggered, Active high, Unmasked
    - Destination = BSP APIC ID (0)
  - Tests: RTE write is readable, timer fires at vector 32.

- [ ] **7.6.7 — Add APIC EOI to timer handler**
  - The `timer_handler` now calls `arch_x86_64::apic::eoi()` which sends APIC
    EOI when the APIC is active, or PIC EOI in PIC-only mode.
  - The generic `interrupt_handler_c` also uses `crate::apic::eoi()`.
  - Verified: `echo` command works in shell with no interrupt errors.

- [ ] **7.6.8 — Verify NMI fix and basic command stability**
  - After initialization, timer fires at vector 32 via I/O APIC as a regular
    maskable interrupt (respects IF). Confirmed by `echo hello` running cleanly.
  - No `[ERROR] INT` messages during boot or basic command execution.
  - `ls` crashes due to a separate VFS/MFS page table issue (user-space
    accesses through IPC). This is a Phase 9/10 bug, not related to APIC.
  - Integration test: `echo hello` works; `ls` needs VFS fix.

- [ ] **7.6.9 — Interrupt router abstraction**
  - Create `crate::arch_x86_64::apic` module:
    - `ApicMode` enum (PIC-only, xAPIC, x2APIC)
    - `Apic::detect()` — detect available mode
    - `Apic::init()` — full init (mask I/O APIC, configure LVT, set SVR)
    - `Apic::eoi()` — send EOI to the active controller
    - `Apic::io_apic_redirect(irq, vector, apic_id)` — configure RTE
  - Tests: Unit tests for mode detection, register access (via mock).

### Implementation notes

- APIC registers are accessed via MMIO at the base address from IA32_APIC_BASE MSR.
- In x2APIC mode (bit 10 of IA32_APIC_BASE), use RDMSR/WRMSR with register
  number = 0x800 + (offset >> 4) instead of MMIO.
- QEMU uses xAPIC by default (`-cpu qemu64`); x2APIC with newer CPU models.
- I/O APIC is typically at physical address 0xFEC00000.
- I/O APIC access uses two MMIO registers: IOREGSEL (offset 0x00) selects
  the register index; IOWIN (offset 0x10) reads/writes the value.
- All xAPIC register offsets are 16-byte aligned.
- Reference: Intel SDM Vol 3A, Chapters 10-11.

### Source files

- Implementation: `crates/arch-x86_64/src/apic.rs`
- Tests: in-module unit tests + `crates/arch-x86_64/tests/apic_tests.rs`
- Integration: update `crates/kernel-boot/src/main.rs` to call `apic::init()`

---

## Phase 8: x86_64 Kernel Architecture-Specific Code

**Goal**: Implement the x86_64-specific kernel code. This is **the primary delivery target** and requires significant new work beyond what Minix 3.3.0 provides (no x86_64 in Minix 3.3.0).

### x86_64 vs i386 differences that must be handled:

| Area | i386 (Minix 3.3.0) | x86_64 (port target) | Notes |
|------|---------------------|----------------------|-------|
| **Syscall** | `int 0x80` (32-bit) | `syscall`/`sysret` (fast) | Different registers: RCX/R11 for syscall; no RSP save |
| **Page tables** | 2-level (PDE/PTE) | 4-level (PML4/PDPT/PD/PT) | 5-level optional (LA57) |
| **Address space** | 4GB (3GB/1GB split) | 256TB+ (user: 47 bits) | Huge virtual address space |
| **GDT** | 8-bit selectors, 32-bit descriptors | 16-bit selectors, 64-bit descriptors | Different segment format |
| **TSS** | 104 bytes | 256 bytes | rsp0/1/2, ss0, ist1-6, msr_base, debug, cr3, cr8, efi, rflags |
| **Segment limits** | 32-bit | 64-bit (with EXT/G bit) | Large pages via PXE/PDE |
| **IDT** | 8-byte descriptors | 16-byte descriptors | Different format |
| **Interrupts** | PIC/APIC legacy | APIC/x2APIC | x2APIC mode preferred |
| **Stack frame** | 32-bit registers | 16 registers (RAX-R15) | More regs to save/restore |
| **Stack alignment** | 4-byte | 16-byte (ABI) | Must maintain |
| **Calling convention** | cdecl | System V AMD64 ABI | RCX/RDX/R8/R9 for args |
| **Kernel stack** | 8KB-16KB | 16KB+ (must be 4K-aligned) | Must be 4K aligned for `swapgs` |

### Tasks

- [ ] **8.1 — Implement `crates/arch-x86_64/` — x86_64 kernel arch code**
  - **New crate** (not ported from Minix 3.3.0 — adapted from i386 with significant changes):
  - `prot_init.rs` — protection (page fault handler, IDT setup, GDT/LDT)
  - `cstart.rs` — early boot (multiboot2 → kernel transition, **adapt for UEFI/multiboot2**) 
  - `arch_proc.rs` — architecture-specific process setup (64-bit TSS, PDA)
  - `hw_intr.rs` — hardware interrupt setup (APIC/x2APIC for x86_64)
  - `direct_utils.rs` — direct video/serial I/O
  - `arch_syscall.rs` — `syscall`/`sysret` entry point assembly, IPC syscall handlers
  - `kern_stack.rs` — kernel stack setup (16KB, 4K-aligned), boot pool of 20 stacks
  - `idt.rs` — IDT setup (16-byte descriptor format, 256 entries)
  - `vmcall.rs` — VM call interface
  - **`asm.rs` — exec target mechanism**: `EXEC_TARGET_RIP` / `EXEC_TARGET_RSP` statics
    checked in `syscall_entry` after dispatch; if set, returns to new binary instead
    of saved RCX. `restore()` updated to use user RSP from rdi slot (offset 32) and
    removed segment register restore (not needed for 64-bit).
  - **`cpulocals.rs` — GS base layout**: Added `kernel_stack` (gs:0x0) and `user_rsp`
    (gs:0x8) as first two `CpuLocalVars` fields for `swapgs` compatibility. All field
    offset constants updated accordingly.
  - **`boot_proc.rs`**: Boot stack pool increased from 16 to 20 (`BOOT_STACK_COUNT`)
  - **`kern_stack.rs`**: Pool increased from 256 KB to 320 KB
  - Tests: 224+ tests passing, boot sequence initializes GDT/IDT/TSS correctly
    (boot entry + cstart::boot_setup ready for reintegration)

- [ ] **8.2 — Adapt `sys/arch/i386/` for x86_64**
  - `conf/GENERIC_x86_64` — Kernel config: SMP, APIC/x2APIC, multiboot2,
    paging levels, process table sizes, VM/CpGrant/SAFE_COPIES options,
    device drivers (vga, serial, pic, apic, ioapic, mfs)
  - `conf/stand.ldscript` — x86_64 bootloader linker script (elf64,
    multiboot section, 64-byte alignment)
  - `include/x86_64/GENERIC_x86_64.hints` — Hardware hints: APIC base
    (0xFEE00000), I/O APIC (0xFEC00000), PIC ports (0x20/0xA0), IRQ-to-
    vector mappings (32-47), COM1/COM2 serial, VGA frame (0xB8000)
  - Phase 2.1 already adapts all include/ headers (param.rs, vmparam.rs,
    segments.rs, tss.rs, pcb.rs, frame.rs, etc.)
  - Tests: 4 config parser tests (generic_x86_64_parses_successfully,
    generic_x86_64_has_all_expected_options, comments/blanks handling)

- [ ] **8.3 — Handle assembly references to `struct proc`**
  - `crates/kernel/src/sched/proc.rs`: Added 40+ `PROC_*_OFFSET` constants using
    `core::mem::offset_of!(Proc, ...)` for all fields
  - `crates/arch-x86_64/src/proc_offsets.rs`: Cross-crate offset module with:
    - 44 proc field offsets (p_nr through p_signal_received)
    - 17 segment register offsets (gs=0, fs=8, ... ss=120)
    - Size constants (STACKFRAME_SIZE=128, SEGFRAME_SIZE=32, MESSAGE_SIZE=64)
    - Compile-time assertions (PROC_SIZE bounds, offset contiguity)
  - `crates/kernel/Cargo.toml`: Added kernel as arch-x86_64 dependency
  - Tests: 6 tests (all_proc_offsets_match_rust_layout, segment_register_offsets_contiguous,
    stackframe_size_is_128, proc_size_is_reasonable, message_endpoint_clock_sizes,
    proc_struct_field_order_valid)

- [ ] **8.4 — 64-bit page table management**
  - Implemented in pre-existing `pagetable.rs` + `pmap.rs`:
  - 4-level page table (PML4 → PDPT → PD → PT) with constants and types
  - Physical memory allocator with direct mapping
  - Page fault handling for x86_64 (CR2, error code format in `prot_init.rs`)
  - Tests: vmparam tests verify kernel/user address constants and page alignment

- [ ] **8.5 — 64-bit syscall ABI**
  - Implemented in `arch_syscall.rs`:
  - `syscall`/`sysret` entry/exit via `LSTAR`/`STAR` MSR setup
  - **Fixed STAR MSR values**: SYSCALL CS=0x08 (kernel code), SS=0x10 (kernel data);
    SYSRET CS=0x1B (user code, DPL=3) — corrected from incorrect GUCODE_SEL values
  - Syscall table registration and dispatch (320 entries, `SYS_MAX`=50)
  - **Current process tracking**: `CURRENT_PROC` static + `set_current_proc()`/`current_proc()`
  - **IPC syscall handlers** (46-49): `ipc_send_handler`, `ipc_receive_handler`,
    `ipc_sendrec_handler`, `ipc_notify_handler` — route through `do_sync_ipc()`
    via the in-kernel server dispatch mechanism (Phase 4.4)
  - Register layout: RCX (return), R11 (flags)
  - `vmcall.rs` — VM call interface for VM monitor communication
  - **`asm.rs` updates**: Fixed syscall_entry argument register mapping (arg order was
    inverted). Added exec target check — if `EXEC_TARGET_RIP` is non-zero after dispatch,
    clears the globals, sets R11 to safe RFLAGS, and returns to the new binary.
    `restore()` updated for correct user stack handling.
  - 7+ tests: vmcall tests, STAR MSR value computation (syscall CS, sysret CS),
    handler registration and dispatch

- [ ] **8.6 — Fix bugs discovered during first userspace boot (QEMU debug)**
  - Debugging `restore()` → iretq → ring-3 → `syscall` crash uncovered:
  - **`IA32_KERNEL_GS_BASE` MSR constant wrong**: The constant was `0xC0000109` but
    Intel SDM Vol 4 Table 2-7 specifies `0xC0000102`. `swapgs` swapped GS base with
    an uninitialized MSR, so `gs:0x0` read from virtual address 0 (identity-mapped
    to physical 0 = real-mode IVT), returning garbage `0xF000FF53` as the kernel
    stack pointer → triple fault. **Root cause**: copy-paste error from an AMD or
    processor-specific MSR number.
    - Fix: `crates/arch-x86_64/src/cpu_msr.rs` — changed constant + test
    - Covered by: `msr_constants` test now asserts `0xC0000102` with Intel SDM comment
  - **GDT code segment D/B flag wrong for long mode**: Both kernel and user code
    descriptors used flags `0x5F` = `D/B=1, L=1`. Per Intel SDM Vol 3 Section 3.4.5.1,
    when L=1, D/B must be 0. QEMU treated this as `CS32` (compatibility mode),
    so iretq returned to 32-bit mode instead of 64-bit → garbage instruction
    fetch → #GP → triple fault.
    - Fix: Changed to `0xAF` = `G=1, D/B=0, L=1` in both `BOOT_GDT_VALUE` constant
      and the runtime `GDT_BUF` construction in kmain
    - Covered by: Corrected `gdt_decode_byte6()` bit shifts. Tests assert `!d_or_b`
      with `long` and spec reference.
  - **User stack outside RAM-backed physical memory**: Stack base was `0x3FF00000`,
    which identity-maps to physical `0xFFE00000` (PD[511]). With QEMU `-m 256M`,
    physical RAM only extends to `0x0FFFFFFF`. Stack accesses silently corrupted
    or returned garbage.
    - Fix: `crates/kernel-boot/src/boot_init.rs` — moved stack base to `0x0FE00000`,
      well within the 256MB RAM range
    - Covered by: `user_stack_within_ram` test asserts stack end < RAM_TOP
  - **PM_EXEC_NEW constant mismatch**: `minix-std` defined it as `PM_BASE + 30` (0x01E)
    but `servers/pm.rs` defines it as `PM_BASE + 43` (0x02B). Kernel SUSPEND handler
    checked for 0x02B, so exec target never got set → exec returned without loading
    a new binary → init called exit → HLT.
    - Fix: `crates/minix-std/src/process.rs` — changed to `PM_BASE + 43`
    - Covered by: `pm_call_numbers_are_correct` and `exec_message_fields` tests
  - **SLOT_FREE never cleared in boot_create_procs**: `proc_init` sets `SLOT_FREE`
    on all process slots, `boot_create_procs` never cleared it. Deadlock detection
    walked process chain and hit empty slots with SLOT_FREE set → assertion panic.
    - Fix: `crates/kernel/src/sched/table.rs` — add `p.p_rts_flags -= SLOT_FREE`
    - Covered by: `boot_create_procs_clears_slot_free` test
  - **Exec stack also outside RAM**: SUSPEND handler for PM_EXEC used `0x3F000000`
    (same class of bug as user stack). Moved to `0x0FE00000`.
    - Fix: `crates/kernel/src/ipc.rs`
    - Covered by: same `user_stack_within_ram` test (shared constant)
  - **SYS_READ handler missing**: Shell's `read_line()` went through VFS IPC, but
    VFS has no registered dispatch handler → IPC blocked forever.
    - Fix: Added direct serial port read handler (syscall 8) + `minix_rt::read()`
    - Not covered by host tests (requires QEMU for serial I/O)
  - **All 5 fixes now have test coverage** except SYS_READ (needs QEMU).
    357+ tests pass across affected crates.

- [ ] **8.7 — Add boot_init.rs and IPC tests for non-QEMU gaps**
  - `boot_create_procs_clears_slot_free` — iterates all BOOT_IMAGE entries and
    asserts SLOT_FREE is cleared after boot_create_procs
  - `user_stack_within_ram` — statically checks the user/exec stack address is
    within the 256MB RAM region and doesn't overlap the kernel binary
  - `init_idt_full_sets_all_entries_with_correct_cs` — verifies all 256 IDT
    entries have the correct CS selector and handler address
  - `error_code_vectors_are_correct` — verifies the 7 exception vectors that
    push error codes (#DF, #TS, #NP, #SS, #GP, #PF, #AC)
  - Tests: 224+ tests across arch modules; boot sequence initializes GDT/IDT/TSS correctly; syscall dispatch

---

## Phase 19: RISC-V64 Architecture (Bonus Challenge)

**Goal**: Implement a RISC-V64 architecture layer for the port. This is a bonus because Minix 3.3.0 has no RISC-V support — everything must be designed from scratch.

### RISC-V64 considerations:

| Area | x86_64 (Phase 8) | RISC-V64 (Phase 19) |
|------|-------------------|----------------------|
| **Boot** | Multiboot2/UEFI | Device tree + bootloader (QEMU SBI) |
| **Syscall** | `syscall` instruction | `ecall` instruction |
| **Page tables** | 4-level paging | SV39 (3-level) or SV48 (4-level) |
| **Registers** | 16 general + SSE | 32 general + CSR |
| **Interrupts** | APIC/x2APIC | PLIC (Platform Level Interrupt Controller) |
| **Stack** | Fixed kernel stack | Per-CPU stack with shadow stack |
| **MMU** | PTE/PDE | PTE/PMD/PUD (SV39) |

### Tasks

- [ ] **19.1 — Create `crates/arch-riscv64/` crate**
  - Target: `riscv64gc-unknown-minix` (GC = IMACFD = G extension)
  - Custom JSON target spec: `riscv64gc-unknown-minix.json`
  - Tests: Kernel boots in QEMU riscv64; IPC round-trip; fork/exec works; all milestones M1-M12 pass on RISC-V

- [ ] **19.2 — Port/Adapt Minix config headers for RISC-V**
  - Source: `.refs/minix-3.3.0/minix/include/minix/sys_config.h` (configuration)
  - Adapt `param.h`, `vmparam.h` for RISC-V:
  - PAGE_SIZE = 4096, VM_USER_R/VM_USER_W/VM_USER_X regions
  - Virtual address layout: kernel at 0x80000000, user space below
  - Stack frame layout for RISC-V
  - Tests: Kernel boots in QEMU riscv64; IPC round-trip; fork/exec works; all milestones M1-M12 pass on RISC-V

- [ ] **19.3 — Implement RISC-V64 boot code**
  - Device tree parsing (DTB)
  - Multi-hart boot (SBI calls)
  - Page table setup (SV39)
  - Enable MMU and paging
  - Source: adapt `.refs/minix-3.3.0/sys/arch/evbarm/` boot pattern
  - Tests: Kernel boots in QEMU riscv64; IPC round-trip; fork/exec works; all milestones M1-M12 pass on RISC-V

- [ ] **19.4 — Implement RISC-V64 low-level primitives**
  - Assembly: `switch.S` (context switch), `idt.S` (trap table), `cpulocals.S`
  - Rust: trap handler, interrupt controller (PLIC)
  - `mret`/`sret` for returning from traps
  - Tests: Kernel boots in QEMU riscv64; IPC round-trip; fork/exec works; all milestones M1-M12 pass on RISC-V

- [ ] **19.5 — Implement RISC-V64 memory management**
  - Page table management (SV39)
  - TLB management
  - Physical memory allocator for RISC-V
  - Tests: Kernel boots in QEMU riscv64; IPC round-trip; fork/exec works; all milestones M1-M12 pass on RISC-V

- [ ] **19.6 — Implement RISC-V64 syscall ABI**
  - `ecall` entry/exit
  - Register mapping (A0-A7 for args, A0/A1 for return)
  - Signal return via `mret`/`sret`
  - Tests: Kernel boots in QEMU riscv64; IPC round-trip; fork/exec works; all milestones M1-M12 pass on RISC-V

- [ ] **19.7 — RISC-V64 device driver support**
  - PLIC (interrupt controller)
  - UART (serial console)
  - Virtio devices (disk, net)
  - Source: `.refs/minix-3.3.0/minix/drivers/` (port existing drivers with RISC-V adaptations)
  - Tests: Kernel boots in QEMU riscv64; IPC round-trip; fork/exec works; all milestones M1-M12 pass on RISC-V

- [ ] **19.8 — Test RISC-V64 boot in QEMU**
  - QEMU `qemu-system-riscv64 -M virt` boot test
  - All milestones M1-M12 pass on RISC-V

---

### Bonus challenge scope for RISC-V

This phase is **roughly equivalent to Phases 2 + 8 combined** (~8 weeks for a single developer), but with the twist that no Minix 3.3.0 source exists for RISC-V — everything is new design work inspired by:
- Minix 3.3.0 ARM code (`.refs/minix-3.3.0/sys/arch/evbarm/`, `.refs/minix-3.3.0/minix/kernel/arch/earm/`) as architectural reference
- RISC-V spec ( privileged architecture spec for traps, MMU, PLIC)
- QEMU RISC-V machine `virt` as the target platform
- Linux RISC-V kernel for comparison on boot process, page tables, and traps

---

## Phase 9: File System Servers

**Goal**: Port the file system servers that run in user space.

### Tasks

- [ ] **9.1 — Port `minix/fs/mfs/` — Memory File System** (simplest, validation target)
  - Source: `.refs/minix-3.3.0/minix/fs/mfs/` (all files)
  - Implemented in `crates/fs/src/mfs/` (16 modules: buffer, consts, dir, dispatch, file_ops, inode, link_ops, list, misc_ops, mount_ops, protect, read_ops, stat_ops, super_block, time_ops, types, write_ops)
  - Inode cache, buffer cache, superblock management, VFS operations
  - Tests: Filesystem operation round-trips; inode/block bitmap allocation; read/write verification

- [ ] **9.2 — Port `minix/fs/vbfs/` — Virtual Block File System**
  - Source: `.refs/minix-3.3.0/minix/fs/vbfs/vbfs.c` (1 file, ~140 lines)
  - Implemented in `crates/fs/src/vbfs/` (config.rs, server.rs)
  - Thin wrapper around libsffs/libvboxfs; parses CLI options (share, prefix, uid, gid, masks)
  - `#![no_std]` compatible with `extern crate alloc`
  - Tests: Filesystem operation round-trips; inode/block bitmap allocation; read/write verification

- [ ] **9.3 — Port `minix/fs/procfs/` — Process File System**
  - Source: `.refs/minix-3.3.0/minix/fs/procfs/` (12 files: buf.c, cpuinfo.c, main.c, pid.c, root.c, tree.c, util.c, const.h, cpuinfo.h, glo.h, inc.h, proto.h, type.h)
  - Implemented in `crates/fs/src/procfs/` (7 modules: buffer, cpu_info, pid, root, tree, types, util)
  - **types** — Core type definitions: VTreeFS interface types (`Inode`, `IndexT`, `CbData`, `InodeStat`, `FsHooks`), ProcFS `FileEntry`, kernel `ProcEntry`, PM `MprocEntry`, VFS `FprocEntry`, load average types, machine/CPU info types, process state/type constants, configuration constants (`NR_PROCS=256`, `NR_TASKS=32`)
  - **buffer** — Output buffer with skip support (4096-byte static buffer), `buf_init()`, `buf_append()`, `buf_get()`, helper writers (`write_str`, `write_dec`, `write_udec`, `write_hex`), 6 unit tests
  - **cpu_info** — x86 CPU feature flag names, `print_cpu_flags()`, `print_cpu()`, `root_cpuinfo()` handler
  - **util** — `procfs_getloadavg()` load average calculation, placeholder `sys_hz()`/`get_ticks()` syslib wrappers
  - **pid** — Dynamic PID file definitions (`psinfo`, `cmdline`, `environ`, `map`), `pid_psinfo()` full process status in ps format version 0, command line/environment frame parsing, memory map handler
  - **root** — Static root file definitions (`hz`, `uptime`, `loadavg`, `kinfo`, `meminfo`, `dmap`, `cpuinfo`, `ipcvecs`, `mounts`), placeholder handlers for each
  - **tree** — VTreeFS hook implementations (`lookup_hook`, `getdents_hook`, `read_hook`, `rdlink_hook`), process table synchronization (`update_proc_table`, `update_mproc_table`, `update_fproc_table`), dynamic PID directory management, external process table declarations (`PROC`, `MPROC`, `FPROC`)
  - `#![no_std]` compatible with `extern crate alloc`
  - `cargo clippy -p fs -- -D warnings` passes
  - All 138 tests pass (including 6 new procfs buffer tests)

- [ ] **9.4 — Port `minix/fs/iso9660fs/` — ISO 9660 File System**
  - Source: `.refs/minix-3.3.0/minix/fs/iso9660fs/`
  - Implemented in `crates/fs/src/iso9660/` (10 modules: consts, dispatch, inode, misc_ops, mount, path, read_ops, stadir, super_block, types)
  - Core types: `DirRecord` (inode), `ExtAttrRec` (extended attributes), `Iso9660VdPri` (primary volume descriptor), `Dirent` (directory entry)
  - Inode cache with `DIR_RECORDS` (256 entries) and `EXT_ATTR_RECS` (256 entries) static arrays
  - Volume descriptor parsing from ISO 9660 CD medium (sector 16)
  - Path lookup with component-by-component directory traversal
  - Read operations for files and directory entry listing (getdents)
  - Dispatch table mapping VFS/FS call numbers to handlers
  - ISO 9660 date parsing (YYYYMMDDHHMMSS.HH format)
  - File name normalization (version separator `;` trimming, trailing dot removal)
  - 5 unit tests passing (dir record parsing, file name handling, date parsing)
  - `#![no_std]` compatible with `extern crate alloc`
  - `cargo clippy -p fs -- -D warnings` passes

- [ ] **9.5 — Port `minix/fs/ext2/` — ext2 File System**
  - Source: `.refs/minix-3.3.0/minix/fs/ext2/`
  - Port `ext2_lib.c` and `ext2_server.c` separately
  - Inode management, block allocation, directory entries
  - Tests: Filesystem operation round-trips; inode/block bitmap allocation; read/write verification

- [ ] **9.6 — Port `minix/fs/pfs/` — Pipe File System**
  - Source: `.refs/minix-3.3.0/minix/fs/pfs/`
  - Implemented in `crates/fs/src/pfs/` (14 modules: bitmap, buffer, consts, dispatch, inode, link_ops, misc_ops, mod, mount_ops, open_ops, read_ops, stat_ops, types, utility)
  - Inode cache with hash-based lookup, LRU-free list, and bitmap-backed allocation
  - Buffer pool for per-pipe data blocks (4096 bytes each)
  - Dispatch table mapping VFS/FS call numbers to handlers
  - `#![no_std]` compatible with `extern crate alloc`
  - `cargo clippy -p fs -- -D warnings` passes
  - 160 total tests pass (including 13 new pfs tests)

- [ ] **9.7 — Port `minix/lib/libminixfs/` — MINIX native filesystem library**
  - Source: `.refs/minix-3.3.0/minix/lib/libminixfs/` (cache.c, minixfs.h, fetch_credentials.c)
  - Implemented in `crates/libs/src/libminixfs/` (6 modules: buf, cache, constants, errors, inode_bitmaps, superblock)
  - Block cache with LRU eviction, hash table lookup, dirty tracking
  - BlockDevice trait for pluggable I/O
  - Inode/block bitmap allocation (find_first_clear, find_first_set)
  - Superblock read/write, on-disk format types
  - FsError enum with all Minix errno values + Display impl
  - 18 unit tests, all passing
  - `#![no_std]` compatible with `extern crate alloc`

---

## Phase 10: Virtual File System (VFS) Server

**Goal**: Port the VFS server (`.refs/minix-3.3.0/minix/servers/vfs/`) — the central file service.

### Tasks

- [ ] **10.1 — Port `vfs_main.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_main.c`
  - VFS server main loop, request dispatching
  - Created `vfs/mod.rs` with global tables (FPROC, VNODE_TABLE, VMNT_TABLE,
    FILP_TABLE, FILE_LOCK_TABLE, DMAP_TABLE, WORKER_TABLE, SCRATCHPAD_TABLE),
    VFS initialization (`vfs_init()`), and helper functions (`super_user()`,
    `fproc_addr()`, `scratch()`)
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.2 — Port `vfs_kern.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_kern.c`
  - Kernel-facing VFS operations
  - Created `vfs/fproc.rs` with per-process VFS state management, credential
    helpers (`get_fproc()`, `isokendpt()`, `in_group()`)
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.3 — Port `vfs_call.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_call.c`
  - VFS call dispatch (open, close, read, write, ioctl, etc.)
  - Created `vfs/call.rs` with VfsCallTable dispatch mechanism, 40+ message
    type constants (VFS_OPEN through VFS_FSTATFS), handler stubs
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.4 — Port `vfs_dev.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_dev.c`
  - Device file handling
  - Created `vfs/dev.rs` with character device stubs (cdev_open, cdev_close,
    cdev_io, cdev_map, cdev_select, cdev_cancel) and block device stubs
    (bdev_open, bdev_close, bdev_reply, bdev_up, do_ioctl)
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.5 — Port `vfs_mmap.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_mmap.c`
  - Memory-mapped file support
  - Created `vfs/mmap.rs` with VM_MMAP request handler stub and map_vnode()
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.6 — Port `vfs_stat.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_stat.c`
  - File stat operations
  - Types include `StatvfsCache` for cached statvfs fields (16 fields,
    avoids 2KB per mount entry). Full stat dispatch in future task.
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.7 — Port `vfs_misc.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_misc.c`
  - Miscellaneous VFS operations
  - Included in `vfs/types.rs` and `vfs/mod.rs` constants (LABEL_MAX,
    PATH_MAX, FSTYPE_MAX, SYMLOOP) and helper functions
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.8 — Port `vfs_pm.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_pm.c`
  - VFS permission management
  - Permission flags in `vfs/types.rs` (SU_UID, SYS_UID, SYS_GID),
    credential fields in `Fproc` (fp_realuid, fp_effuid, fp_realgid,
    fp_effgid, fp_ngroups, fp_sgroups, fp_umask)
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.9 — Port `vfs_fs.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_fs.c`
  - Filesystem mount operations
  - Created `vfs/mount.rs` with get_free_vmnt(), find_vmnt(),
    lock_vmnt(), unlock_vmnt(), upgrade/downgrade helpers, do_mount/umount stubs
  - Tests: VFS server initialization; device/file operation stubs return expected codes; call dispatch table routing

- [ ] **10.10 — Port `vfs_proc.c`**
  - Source: `.refs/minix-3.3.0/minix/servers/vfs/vfs_proc.c`
  - Process-related VFS operations
  - Created `vfs/vnode.rs` with vnode table management (get_free_vnode,
    find_vnode, dup_vnode, put_vnode, vnode_clean_refs, lock/unlock/upgrade)

### VFS Server Module Structure

Created 13 files in `crates/servers/src/vfs/`:

- `mod.rs` — Global tables (FPROC, VNODE_TABLE, VMNT_TABLE, FILP_TABLE,
  FILE_LOCK_TABLE, DMAP_TABLE, WORKER_TABLE, SCRATCHPAD_TABLE), vfs_init(),
  helper functions
- `types.rs` — Core type definitions (911 lines): Tll, TllAccess, TllStatus,
  Vmnt+StatvfsCache, Fproc, Vnode, Filp, FileLock, Dmap, NodeDetails,
  LookupRes, Lookup, WorkerThread, Scratchpad
- `tll.rs` — Three-level lock implementation with init/lock/unlock/upgrade/
  downgrade/islocked/haspendinglock operations
- `vnode.rs` — Vnode table management with reference counting and locking
- `mount.rs` — Mount table management with allocation, lookup, and locking
- `dev.rs` — Character and block device file operation stubs
- `mmap.rs` — Memory-mapped file support stubs
- `fproc.rs` — Per-process VFS state and credential helpers
- `lock.rs` — Advisory file locking implementation
- `call.rs` — VFS call dispatch table with 40+ message type constants
- `path.rs` — Path resolution and symbolic link handling stubs
- `dmap.rs` — Device-to-driver mapping table management

### Type Layouts (all `#[repr(C)]`)

- **Tll** — Three-level lock (6 fields: t_current, t_owner, t_readonly,
  t_status, t_write, t_serial)
- **Vmnt** — Mount entry (12 fields including m_lock, m_comm, m_mount_path,
  m_mount_dev, m_fstype, m_stats)
- **Fproc** — Per-process state (22 fields including fp_filp[NR_PROCS],
  fp_cloexec_set, fp_sgroups, fp_msg, fp_pm_msg, fp_name)
- **Vnode** — Virtual file node (17 fields including v_lock, v_vmnt,
  v_ref_count, v_fs_count)
- **Filp** — File descriptor table entry (13 fields including filp_select_ops,
  filp_pipe_select_ops)
- **FileLock** — Advisory lock (5 fields: lock_type, lock_pid, lock_vnode,
  lock_first, lock_last)
- **Dmap** — Device map entry (8 fields: dmap_driver, dmap_label,
  dmap_sel_busy, dmap_servicing)
- **WorkerThread** — Worker state (12 fields: w_tid, w_m_in, w_m_out,
  w_task, w_dmap, w_next)

### Constants (from `const.h`)

- NR_FILPS=1024, NR_LOCKS=8, NR_MNTS=16, NR_VNODES=1024,
  NR_WTHREADS=9, NR_DMAPS=64

### Test Coverage

37 new VFS tests (total 131 tests passing, clippy clean):
- `vfs/types.rs` — 11 tests (tll_defaults, vmnt_defaults, fproc_defaults,
  vnode_defaults, filp_defaults, file_lock_defaults, dmap_defaults,
  node_details_defaults, lookup_res_defaults, worker_thread_defaults,
  scratchpad_defaults)
- `vfs/tll.rs` — 7 tests (tll_init_defaults, tll_islocked_*,
  tll_has_pending_*)
- `vfs/vnode.rs` — 8 tests (get_free_vnode, find_vnode, dup_vnode,
  put_vnode, is_vnode_locked, lock_unlock_vnode)
- `vfs/mount.rs` — 6 tests (get_free_vmnt, find_vmnt, mark_vmnt_free,
  lock_unlock_vmnt, lock_vmnt_out_of_bounds)
- `vfs/fproc.rs` — 4 tests (get_fproc_valid/invalid, isokendpt_valid/invalid)
- `vfs/call.rs` — 4 tests (call_table_new_is_empty, call_table_set_and_get,
  call_table_invalid_call, call_table_out_of_bounds_set)
- `vfs/lock.rs` — 5 tests (get_free_lock, find_lock_found/none,
  lock_op_returns_error)
- `vfs/dev.rs` — 5 tests (cdev_open/close, bdev_open/close, do_ioctl)
- `vfs/mmap.rs` — 2 tests (do_vm_mmap, map_vnode)
- `vfs/path.rs` — 3 tests (lookup_init_works, advance_returns_none,
  eat_path_returns_none)
- `vfs/dmap.rs` — 3 tests (get_dmap_found/none, dmap_driver_match_false)
- `vfs/mod.rs` — 4 tests (tables_zero_initialized, fproc_addr_valid/invalid,
  super_user_check)
- `vfs/types.rs` — 8 compile-time size/offset assertions

---

## Phase 11: Device Drivers

**Goal**: Port device drivers (~30+ driver directories, organized as separate crates under `./crates/drivers/`).

### Prioritized order (simplest first):

### Phase 11a: Simple drivers (early integration testing)

**Status: TODO (0%)** — Implemented in `crates/drivers/`.

- [ ] **11a.1 — System drivers** (`crates/drivers/src/system/`)
  - [ ] **GPIO driver** (`gpio.rs`, ~380 lines)
    - Pin modes (input/output), claiming, release
    - Read/write operations, interrupt status
    - BeagleBone-specific GPIO configurations (USR0/USR1, buttons, LCD_EN)
    - `gpio_global_pin(bank, pin)` and `gpio_parse_pin(global_pin)` helpers
  - [ ] **Kernel log driver** (`klog.rs`, ~305 lines)
    - 4096-byte circular buffer
    - Append, read, write with overflow handling
    - Non-blocking read support (-EAGAIN)
  - [ ] **Random number generator** (`random.rs`, ~378 lines)
    - 16 entropy sources with derivative-based quality detection
    - 32 entropy pools, AES-128 ECB-based PRNG
    - Minimum 256 samples before reseed
    - External entropy injection via `random_putbytes()`

- [ ] **11a.2 — Clock drivers** (`crates/drivers/src/clock/`)
  - [ ] **CMOS/RTC driver** (`rtc.rs`, ~415 lines)
    - CMOS I/O port access (0x70/0x71)
    - BCD/binary conversion, update-in-progress sync
    - Time get/set with consistency checking
    - Power-off via keyboard controller (port 0xB2)
    - `RtcTime` struct with year conversion (2000 + year field)

- [ ] **11a.3 — EEPROM drivers** (`crates/drivers/src/eeprom/`)
  - [ ] **CAT24C256 driver** (`cat24c256.rs`, ~480 lines)
    - 256K-bit (32KB) I2C EEPROM support
    - Valid I2C addresses: 0x50-0x57
    - Page-aligned writes (16 bytes/page)
    - Chunked reads (128 bytes/chunk)
    - I2C ioctl execution structure

- [ ] **11a.4 — Bus drivers** (`crates/drivers/src/bus/`)
  - [ ] **I2C driver** (`i2c.rs`, ~370 lines)
    - 10-bit addressing (1024 devices)
    - Device reservation table with endpoint tracking
    - Hardware-specific process callback framework
    - Reservation validation and conflict detection
  - [ ] **PCI driver** (`pci.rs`, ~612 lines)
    - PCI configuration space access (extern "C" I/O ops)
    - Device enumeration (vendor/device IDs, BARs)
    - BAR resource management (6 BARs per device)
    - ACL entries for driver access control
    - `PciState` with 32 ACLs and device array
  - [ ] **PCI config-space access** (`crates/arch-x86_64/src/pci.rs`, ~114 lines)
    - Standard x86 PCI config mechanism (0xCF8/0xCFC ports)
    - 8/16/32-bit read/write via port I/O (`inl`/`outl` from asm.rs)
    - Byte-aligned reads/writes within 32-bit config registers
    - 2 tests covering port constants and address encoding
  - [ ] **TI1225 CardBus driver** (`ti1225.rs`, ~440 lines)
    - TI1225 PCI-to-PCI bridge driver
    - CSR (Control Status Register) handling
    - Card detection, power management, hot-plug events
    - Voltage detection, bridge reset, bus rescanning

- [ ] **11a.5 — Architecture support** (`crates/arch-x86_64/`)
  - [ ] I/O port access (`inb`/`outb`)
  - [ ] Interrupt enable/disable (`intr_enable`/`intr_disable`)

### Phase 11b: Storage drivers

**Dependencies**: Requires PCI driver (11a.4) and I2C driver (11a.4) for storage controller enumeration.

- [ ] **11b.1 — `minix/drivers/storage/ahci/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/ahci/`
  - AHCI driver — real PCI wiring (MMIO BAR5, `init_with_pci()`, `read_cap()`, DMA buffer allocation)
  - Fixed: `is_atapi()`, `is_ata()`, `ncq_depth()`, `long_logical_sectors()`, `probe()`, `map_minor_to_port()`
  - 14/14 tests passed (previously 7/14; fixed `is_atapi()`/`is_ata()` GCAP encoding, NCQ depth extraction, MMIO probe, device mapping)

- [ ] **11b.2 — `minix/drivers/storage/at_wini/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/at_wini/`
  - IDE/PATA driver (major driver, heavily tested) — stub ported (1/1 ignored for zeroed defaults)

- [ ] **11b.3 — `minix/drivers/storage/floppy/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/floppy/`
  - Floppy driver — stub ported (1/1 ignored for density table defaults)

- [ ] **11b.4 — `minix/drivers/storage/ramdisk/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/ramdisk/`
  - RAM disk driver — stub ported (28/28 passed)

- [ ] **11b.5 — `minix/drivers/storage/virtio_blk/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/virtio_blk/`
  - Virtio block driver — stub ported (15/15 passed)

- [ ] **11b.6 — `minix/drivers/storage/vnd/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/vnd/`
  - Virtual disk driver — stub ported (4/16 passed; 12 ignored for ENODEV stub)

- [ ] **11b.7 — `minix/drivers/storage/filter/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/filter/`
  - Storage filter driver — 18/18 passed (fixed CRC32 final XOR, MD5 copy slice length, filter driver retry defaults)

- [ ] **11b.8 — `minix/drivers/storage/mmc/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/mmc/`
  - MMC driver — 25/25 passed (added `Disconnect` card state, fixed default block_size to 512)

- [ ] **11b.9 — `minix/drivers/storage/memory/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/memory/`
  - Memory storage driver — stub ported (12/12 passed)

- [ ] **11b.10 — `minix/drivers/storage/fbd/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/storage/fbd/`
  - Framebuffer disk driver — stub ported (8/8 passed)

- [ ] **11b.13 — Stub fixes: vnd, at_wini, floppy**
  - Source: `crates/drivers/src/storage/{vnd,at_wini,floppy}.rs`
  - vnd: Fixed `set_fd()` ENODEV — removed too-strict `open_count` guard for unconfigured devices
  - at_wini: Fixed `Default` impl — set `max_count` to `AT_WINI_MAX_SECS` (256) instead of zeroed
  - floppy: Fixed `Default` impl — set `density_index` to 3 (1.44" HD) instead of zeroed
  - klog: Fixed `vec![]` shadowing by adding `use self::alloc::vec` in x86 test module
  - pci: Fixed `test_stubs` module guard (`#[cfg(not(feature = "x86"))]`) to avoid symbol conflicts

- [ ] **11b.11 — PIC (8259A) wiring**
  - Source: `crates/arch-x86_64/src/hw_intr.rs`
  - `remap_pic()` — full ICW1–4 programming: vector base (0x20 master / 0x28 slave), cascade config, 8086 mode
  - `set_irq_vector()` — xAPIC/x2APIC-aware IRQ vector assignment
  - `mask_irq()` / `unmask_irq()` — APIC LVT mask bit (x2APIC) or PIC IMR bit (xAPIC)
  - `enable_apic()` made public
  - Tests: 232 passed, 0 failed, 5 ignored (arch-x86_64 crate)

- [ ] **11b.12 — Storage DMA API**
  - Source: `crates/drivers/src/storage/dma.rs`
  - `alloc_dma_buf(n)` — wraps `PhysicalAllocator::alloc_contig()` for PRD tables
  - `free_dma_buf(buf)` — delegates to `free_contig()`
  - `dma_buf_phys()`, `dma_buf_page_count()`, `dma_buf_size()` — accessors
  - `DmaBuffer` — RAII wrapper with `Drop` auto-free
  - Stub impl for non-x86 builds (returns `None`/zero)
  - Added `dma` module to storage `mod.rs`
  - Tests: 3 passed (page size, max pages, stub behavior)

- [ ] **11b.13 — PIT timer + PIC remap + timer ISR** (kernel-boot)
  - PIT channel 0 programmed at 100 Hz (mode 3, square wave)
  - PIC remapped via inline asm with I/O delays (not naked `outb`)
  - Timer ISR: full register save/restore, calls `timer_handler`, EOI, `iretq`
  - `TICK_COUNT` incremented in handler, polled by main loop
  - Fixed `lidt()` — removed `options(nomem)` so descriptor buffer flush works
  - Fixed `hlt` — removed `options(nomem)` so `TICK_COUNT` read isn't hoisted
  - Fixed CS selector — read dynamically via `mov cs` (0x08 for trampoline, 0x18 for stage2)
  - Skipped broken kernel GDT reload in `boot_setup()` (struct layout bug)
  - Heartbeat dot every 100 ticks via `hlt` loop
  - Works with both `just run` (SeabIOS `-kernel`) and `just run-img` (disk image)
  - Source: `crates/drivers/src/storage/ahci.rs`
  - `is_atapi()`: Fixed GCAP matching (0x4000|0xC000) for ATAPI types
  - `is_ata()`: Fixed to reject zeroed config, accept pure-ATA (GCAP=0x0) and ATA-bit-only (0x8000)
  - `ncq_depth()`: Fixed to use lower 5 bits (not shifted upper bits)
  - `long_logical_sectors()`: Already correct (no change needed)
  - `probe()`: Now correctly reads HBA Cap via MMIO and populates has_ncq/has_clo
  - `map_minor_to_port()`: Fixed fallback to default port mapping when no devices detected
  - 14/14 ATA IDENTIFY tests now pass

- [ ] **11b.15 — MMC/SD card detection** (hardware-dependent)

### Phase 11c: Network drivers

**Dependencies**: Requires PCI driver (11a.4) for network device enumeration, DMA API (11b.12), PIC wiring (11b.11).

- [ ] **11c.infra — Network driver infrastructure** (724 lines, 50 tests)
  - `crates/arch-x86_64/src/mmio.rs` — 194 lines, 6 tests
    - `mmio_read8/16/32/64()`, `mmio_write8/16/32/64()` — volatile MMIO access
    - `mmio_write32_le()`, `mmio_read32_le()` — little-endian byte-wise access
    - `mmio_read8_safe()` — read with error flag
  - `crates/arch-x86_64/src/irq.rs` — 220 lines, 4 tests
    - `irq_enable()`, `irq_disable()`, `irq_ack()` — high-level IRQ management
    - `io_read32/16/8()`, `io_write32/16/8()` — I/O port helpers for rtl8139/dp8390
    - `IrqState` — per-device IRQ state tracker
  - `crates/virtio/` (new crate) — 671 lines, 10 tests
    - **`lib.rs`** (497 lines): `VirtioDeviceType` (22 types), feature flags, status bits, `VirtioDevice` trait, `QueueAlloc`/`QueueState`/`VirtioQueue`, notification helpers
    - **`x86.rs`** (174 lines): MMIO register offsets, hardware primitives for virtio backend
  - **Stub fixes** (7 → 0 failures):
    - `dec21140A`: Fixed `TEST_SROM` — MAC was at byte 5 instead of offset 20
    - `e1000`: Fixed `eeprom_bits` masks — `0xFFFF0000` for DATA, `0x0000FF00` for ADDR
    - `rtl8139`: Fixed `interrupt_bits` — changed `& != 0` to `& == 0` (different bits don't overlap)
    - `rtl8169`: Same fix as rtl8139
  - **All stubs**: Created with driver-specific constants/structs, `#[expect(...)]` for naming conventions, comprehensive test modules

- [ ] **11c.1 — Network stubs (13 drivers)** — all stubs created, 403+ driver tests pass
  - `crates/drivers/src/network/virtio_net.rs` — 812 lines (stub with full constants/features)
  - `crates/drivers/src/network/atl2.rs` — 363 lines
  - `crates/drivers/src/network/dec21140A.rs` — 649 lines (full constants/register offsets)
  - `crates/drivers/src/network/e1000.rs` — 442 lines
  - `crates/drivers/src/network/fxp.rs` — 453 lines
  - `crates/drivers/src/network/lance.rs` — 430 lines
  - `crates/drivers/src/network/rtl8139.rs` — 421 lines
  - `crates/drivers/src/network/rtl8169.rs` — 572 lines
  - `crates/drivers/src/network/dp8390.rs` — 436 lines
  - `crates/drivers/src/network/dpeth.rs` — 323 lines
  - `crates/drivers/src/network/uds.rs` — 395 lines
  - `crates/drivers/src/network/orinoco.rs` — 338 lines
  - `crates/drivers/src/network/lan8710a.rs` — 457 lines
  - `crates/drivers/src/network/mod.rs` — module declarations

- [ ] **11c.2 — `crates/drivers/src/network/virtio_net.rs`** (full implementation)
  - Source: `.refs/minix-3.3.0/minix/drivers/net/virtio_net/`
  - Depends on: virtio transport layer (11c.infra)
  - **Hardware-specific features:**
    - `impl VirtioDevice for VirtioNetDevice` — bridges stub with virtio transport
    - `init()` — full virtio device status transitions (RESET → ACKNOWLEDGE → FEATURES_OK → DRIVER_OK), MMIO feature negotiation via `virtio::x86` primitives
    - `open()` — DMA queue ring allocation (`alloc_dma_buf`), per-queue `QueueAlloc` setup with descriptor/avail/used ring offsets, device ready status
    - `close()` — DMA buffer cleanup, device reset (FAILED → RESET)
    - `allocate_queues()` — RX/TX/CTRL queue setup with proper ring layout, DMA allocation, and MMIO queue size programming
    - `handle_irq()` — `has_irq()` check + `ack_irq()` via MMIO
    - `refill_rx_queue()` — submits up to BUF_PACKETS/2 free packets to RX
    - `check_queues()` — processes completed RX/TX operations
    - `handle_write()` — DL_WRITEV_S handler
    - `handle_read()` — DL_READV_S handler
    - `handle_conf()` — DL_CONF handler, sets DRIVER_OK status
    - `handle_getstat()` — DL_GETSTAT_S handler
    - `main_loop()` — main event loop (refill + receive dispatch stub)
  - **Infrastructure changes:**
    - `virtio` crate: `pub mod x86` (was private), `Debug` on `VirtioQueue`
    - `drivers` crate Cargo.toml: virtio dep enables `x86` feature
  - **Tests**: 58 pass (8 new), 3 ignored
  - ~680 lines C source → ~1800+ lines Rust

- [ ] **11c.3 — `crates/drivers/src/network/atl2.rs`** (full implementation)
  - Source: `.refs/minix-3.3.0/minix/drivers/net/atl2/`
  - Intel 82573E / Attansic L2 driver
  - **Implemented:**
    - `init()` — MMIO base setup, VPD MAC read stub
    - `stop()` — disable interrupts, stop MAC RX/TX
    - `reset()` — soft reset with wait loop
    - `setup()` — PCIE init, PHY enable, ring buffer config, MAC setup
    - `tx_advance()` — TX descriptor/status ring processing, packet count
    - `rx_advance()` — RX descriptor ring processing, packet availability
    - `handle_irq()` — ISR read, TX/RX processing, ISR clear
    - `get_link_status()` — PHY stat read, autonegotiation check
    - `set_mode()` — promiscuous/multicast/broadcast configuration
    - MMIO helpers (volatile read8/16/32, write8/16/32)
  - **New types:**
    - `Atl2TxStatus` — TX status descriptor (64-bit)
    - `Atl2TxDesc` — TX descriptor (16 bytes)
    - `Atl2RxD` — RX descriptor (8 bytes)
    - `Atl2DmaBuf` — DMA buffer tracking
    - `Atl2RingState` — per-ring tail/count management
    - `Atl2Stats` — full network statistics struct
  - **New constants:** 100+ register offsets, bit masks, PHY registers
  - **Tests:** 19 pass
  - ~1293 lines C source → ~1300+ lines Rust

- [ ] **11c.4 — `crates/drivers/src/network/e1000.rs`** (full implementation)
  - Source: `.refs/minix-3.3.0/minix/drivers/net/e1000/e1000.c` (~1208 lines C source)
  - Intel Pro/1000 Gigabit Ethernet driver
  - **Implemented:**
    - `init()` — MMIO base setup, hardware init
    - `stop()` — reset HW, disable interrupts
    - `reset_hw()` — soft reset with wait loop
    - `setup()` — clear MTA, clear stats, enable ASDE, configure flow control, init_addr, init_buf, enable interrupts
    - `tx_advance()` — TX descriptor ring processing, packet count
    - `rx_advance()` — RX descriptor ring processing, packet availability
    - `handle_irq()` — ICR read, LSC/TX/RX processing, ICR clear (W1C)
    - `get_link_status()` — status register read, link speed decoding
    - `set_mode()` — promiscuous/multicast/broadcast configuration via RCTL
    - `get_stats()` — hardware counter reads (CRCERRS, RXERRC, MPC, TPR, TPT, COLC)
    - `eeprom_eerd()` — EEPROM read via EERD register
    - `eeprom_ich()` — EEPROM read via ICH flash registers
    - `init_addr()` — MAC address from EEPROM + RAL/RAH setup
    - `init_buf()` — RX/TX descriptor ring allocation + register programming
    - MMIO helpers (volatile read8/16/32, write8/16/32)
    - Register bit helpers (reg_set, reg_unset)
  - **New types:**
    - `E1000RxDesc` — RX descriptor (16 bytes, `#[repr(C)]`)
    - `E1000TxDesc` — TX descriptor (16 bytes, `#[repr(C)]`)
    - `IchFlashStatus` — ICH flash status register bit layout
    - `IchFlashCtrl` — ICH flash control register bit layout
    - `E1000DmaBuf` — DMA buffer tracking
    - `E1000RingState` — per-ring tail/count management
    - `E1000Stats` — full network statistics struct
    - `E1000LinkStatus` — link status from device
  - **New constants:** 110+ PCI device IDs (8254x/8257x/82575/82576/ICH8/ICH9/ICH10/PCH), register offsets, stat registers (CRCERRS, RXERRC, MPC, COLC, TPR, TPT), descriptor status/error/command bits, ICH flash registers
  - **Tests:** 61 pass
  - ~1208 lines C source → ~2085 lines Rust

- [ ] **11c.5 — `crates/drivers/src/network/dec21140A/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/dec21140A/`
  - DEC 21140 driver

- [ ] **11c.6 — `crates/drivers/src/network/dp8390/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/dp8390/`
  - NS8390 driver (ISA, I/O port-based)

- [ ] **11c.7 — `crates/drivers/src/network/fxp/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/fxp/`
  - Intel Fast Ethernet driver

- [ ] **11c.8 — `crates/drivers/src/network/lance/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/lance/`
  - AMD Lance driver

- [ ] **11c.9 — `crates/drivers/src/network/rtl8139/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/rtl8139/`
  - Realtek 8139 driver (I/O port-based, ~2380 lines)

- [ ] **11c.10 — `crates/drivers/src/network/rtl8169/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/rtl8169/`
  - Realtek 8169 driver (~1928 lines)

- [ ] **11c.11 — `crates/drivers/src/network/uds/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/uds/`
  - UDP over serial driver (~1827 lines)

- [ ] **11c.12 — `crates/drivers/src/network/orinoco/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/orinoco/`
  - Wireless driver (~2559 lines)

- [ ] **11c.13 — `crates/drivers/src/network/dpeth/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/dpeth/`
  - DP83815 driver (~3330 lines)

- [ ] **11c.14 — `crates/drivers/src/network/lan8710a/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/net/lan8710a/`
  - LAN8710A PHY driver (~1246 lines)

### Phase 11d: Input & display drivers

**Dependencies**: Requires GPIO driver (11a.1) for keyboard/mouse hardware interface.

- [ ] **11d.1 — `minix/drivers/input/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/hid/pckbd/`
  - Keyboard driver (PS/2), mouse driver (PS/2)
  - `crates/drivers/src/input/` — PS/2 keyboard & mouse driver
    - `keyboard.rs` — Scancode translation, shift/Caps Lock tracking, Colemak layout
    - `mouse.rs` — PS/2 3-byte packet processing, button state, signed delta
    - `controller.rs` — Keyboard controller I/O (ports 0x60/0x64)
    - `driver.rs` — `InputDriver` struct unifying keyboard + mouse
    - `scanmap.rs` — `SCANMAP_NORMAL`, `SCANMAP_COLEMAK`, `SCANMAP_ESCAPED`
    - `constants.rs` — All PS/2 constants from `pckbd.h` + `input.h`
  - Shift modifier tracking (left/right shift press/release)
  - First-class Colemak keyboard layout support
  - `should_shift()` helper for console character generation
  - 532 tests passing across the entire crate (input subsystem covered)

- [ ] **11d.2 — `minix/drivers/video/fb/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/video/fb/`
  - VESA framebuffer driver
  - `crates/drivers/src/video/fb.rs` — FramebufferDriver with open, close, read, write, ioctl
  - `#[repr(C)]` types: `FbVarScreeninfo`, `FbFixScreeninfo`, `FbBitfield`, `FbDevice`
  - 28 unit tests

- [ ] **11d.3 — `minix/drivers/video/tda19988/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/video/tda19988/`
  - TDA19988 video driver
  - `crates/drivers/src/video/tda19988.rs` — Tda19988Driver<B: I2cBus>
  - I2C abstraction via `I2cBus` trait with mock
  - 35 unit tests

### Phase 11e: Audio & peripheral drivers

**Dependencies**: Requires PCI driver (11a.4) for audio device enumeration, I2C driver (11a.4) for codec control.

- [ ] **11e.1 — `minix/drivers/audio/es1370/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/audio/es1370/`
  - ES1370 audio driver

- [ ] **11e.2 — `minix/drivers/audio/es1371/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/audio/es1371/`
  - ES1371 audio driver

- [ ] **11e.3 — `minix/drivers/audio/sb16/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/audio/sb16/`
  - Sound Blaster 16 driver

- [ ] **11e.4 — `minix/drivers/printer/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/printer/`
  - Parallel port printer driver

- [ ] **11e.5 — `minix/drivers/tty/tty/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/tty/tty/`
  - Serial port (UART 16550) driver
  - `crates/drivers/src/tty/rs232.rs` (1290+ lines, 24 tests)
  - Full UART 16550 register definitions, baud rate config, 5/6/7/8 data bits,
    parity (None/Odd/Even/Mark/Space), stop bits, FIFO control, interrupt
    management, modem control (DTR/RTS/CTS/DCD), circular input buffer,
    error statistics, break control
  - Wired as `crates/drivers::tty::rs232` behind `x86` feature
  - **Integration with TTY server**:
    - `NR_RS_LINES` changed from 0 → 2 (COM1, COM2)
    - `TtyLine.serial_idx` field for RS-232 ↔ serial port association
    - `tty_serial_input()` — feed received bytes into line discipline
    - `tty_serial_output_pending()` — query pending serial output
    - `rs232_minor_to_index()` / `serial_idx_to_tty_idx()` — minor↔index helpers
    - RS-232 TTY lines initialized with `serial_idx` set during `tty_init()`

- [ ] **11e.6 — `minix/drivers/tty/pty/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/tty/pty/`
  - Pseudo-terminal driver
  - Integrated into `crates/servers/src/tty.rs` (42 tests passing)
  - `Pty` struct with state management, `pty_master_open/close/read/write`,
    `pty_slave_open/close/write`, PTY data transfer via circular buffer
  - TTY lines initialized in `tty_init()` with PTY pairs at minors 128-131
    (TTYPX) and 192-195 (PTYPX)
  - 7 PTY-specific tests: master/slave open/close, data transfer roundtrip

- [ ] **11e.7 — `minix/drivers/hid/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/hid/`
  - Human interface device driver

- [ ] **11e.8 — `minix/drivers/usb/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/usb/`
  - USB core + `usb_hub/`, `usb_storage/`, `usbd/`

- [ ] **11e.9 — `minix/drivers/sensors/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/sensors/`
  - Hardware sensor drivers

- [ ] **11e.10 — `minix/drivers/iommu/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/iommu/`
  - IOMMU driver

- [ ] **11e.11 — `minix/drivers/power/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/power/`
  - Power management driver

- [ ] **11e.12 — `minix/drivers/vmm_guest/`**
  - Source: `.refs/minix-3.3.0/minix/drivers/vmm_guest/`
  - Virtual machine guest driver

### Driver Framework
Each driver must implement the Minix driver protocol:
```rust
trait Driver {
    fn init(&mut self) -> Result;
    fn request(&mut self, req: DriverRequest) -> Result;
    fn shutdown(&mut self);
}
```
- Test: Each driver crate has unit tests + integration tests (mock device)
- Crate: `crates/drivers/` — all Phase 11a drivers implemented (~3,500 lines, 56 tests)
- Architecture: `crates/arch-x86_64/` — I/O port access, interrupt control, PCI config ops
  - PIC (8259A): `remap_pic()`, `set_irq_vector()`, `mask_irq()`, `unmask_irq()` (Phase 11b.11)
- Storage DMA: `crates/drivers/src/storage/dma.rs` — `alloc_dma_buf()`, `free_dma_buf()` wrapping `PhysicalAllocator` (Phase 11b.12)
- Storage stub fixes (Phase 11b.13-14): vnd ENODEV, at_wini defaults, floppy defaults,
  AHCI GCAP/NCQ/IDENTIFY, MMC card states, filter CRC32/MD5 — 250/250 driver tests passing
- Rust 2024 edition, `#![no_std]` for bare-metal compatibility
- Static arrays instead of dynamic allocation (appropriate for kernel)
- `#[repr(C)]` on all ABI-exposed structs for C compatibility

---

## Phase 12: System Servers

**Goal**: Port the core system servers (`.refs/minix-3.3.0/minix/servers/`).

### Tasks

- [ ] **12.1 — SCHED server** (`.refs/minix-3.3.0/minix/servers/sched/`): `main.c`, `schedule.c`, `utility.c`, `proto.h`, `sched.h`, `schedproc.h`
  - Process scheduler server, priority queue management, time quantum enforcement, live update support
  - Tests: Server init; request dispatch; process lifecycle operations; state management

- [ ] **12.2 — RS server** (`.refs/minix-3.3.0/minix/servers/rs/`): `main.c`, `manager.c`, `request.c`, `exec.c`, `error.c`, `memory.c`, `table.c`, `utility.c`, `const.h`, `glo.h`, `inc.h`, `proto.h`, `type.h`
  - Restart Service — process crash recovery, live update coordination, process cloning/restart
  - Tests: Server init; request dispatch; process lifecycle operations; state management

- [ ] **12.3 — PM server** (`.refs/minix-3.3.0/minix/servers/pm/`): `main.c`, `alarm.c`, `exec.c`, `forkexit.c`, `getset.c`, `mcontext.c`, `misc.c`, `profile.c`, `schedule.c`, `signal.c`, `table.c`, `time.c`, `trace.c`, `utility.c`, `const.h`, `glo.h`, `mproc.h`, `pm.h`, `proto.h`, `type.h`
  - Process Manager — fork/exit, exec, signals, timers, UID/GID, ptrace
  - Tests: Server init; request dispatch; process lifecycle operations; state management

- [ ] **12.4 — DS server** (`.refs/minix-3.3.0/minix/servers/ds/`): `main.c`, `store.c`, `inc.h`, `proto.h`, `store.h`
  - Directory Service, resource name publishing/retrieval, subscription management
  - Tests: Server init; request dispatch; process lifecycle operations; state management

- [ ] **12.5 — IPC server** (`.refs/minix-3.3.0/minix/servers/ipc/`): `main.c`, `sem.c`, `shm.c`, `utility.c`, `inc.h`, `ipc.conf`, `proto.h`
  - IPC endpoint management, semaphore support, shared memory
  - Tests: Server init; request dispatch; process lifecycle operations; state management

- [ ] **12.6 — DEVMAN server** (`.refs/minix-3.3.0/minix/servers/devman/`): `main.c`, `bind.c`, `buf.c`, `device.c`, `devinfo.h`, `devman.h`, `proto.h`
  - Device Manager, device binding/unbinding, device enumeration
  - Tests: Server init; request dispatch; process lifecycle operations; state management

- [ ] **12.7 — TTY server**
  - Terminal multiplexing, pseudo-terminal management
  - Tests: Server init; request dispatch; process lifecycle operations; state management

---

## Phase 13: Rust `std` for Minix

**Goal**: Implement Rust `std` for the `x86_64-pc-minix` target. Since the system is
Rust-native, userspace programs use `std` directly instead of C libraries. A minimal
`libc` is provided only for FFI with any remaining C code.

### Architecture

```
userspace crate
     │
     ├── std (Rust's standard library, built with -Z build-std)
     │       └── sys::pal::minix  ← platform abstraction layer
     │               ├── ipc_send/recv/notify  (kernel syscalls)
     │               ├── process lifecycle     (PM server protocol)
     │               ├── file I/O              (VFS server protocol)
     │               ├── memory management      (VM server protocol)
     │               ├── time/sleep            (CLOCK server protocol)
     │               ├── signal handling       (PM server protocol)
     │               ├── networking            (LWIP driver protocol)
     │               └── device I/O            (driver message protocol)
     │
     └── minix-rt (runtime: _start, panic handler, allocator)
```

### Tasks

- [ ] **13.1 — `crates/minix-rt` runtime crate**
  - `_start` entry point (naked asm, ABI-compatible with kernel exec)
  - Panic handler (format + write to stderr, abort)
  - Bump allocator backed by `brk` syscall (`BrkAllocator`)
  - Syscall wrappers (`syscall0`–`syscall6` via `syscall` instruction)
  - `exit()`, `write()`, `getpid()`, `sbrk()` primitives
  - Tests: syscall numbers, alignment math, function signatures

- [ ] **13.2 — `crates/minix-std` syscall layer**
  - IPC primitives: `send`, `receive`, `sendrec`, `notify`, `senda` via `syscall`
  - Endpoint constants: all well-known system server endpoints, `ANY`/`NONE`/`SELF`
  - Error types: `MinixErr` with Display, `from_syscall()`, 20+ error constants
  - Grant table: `GrantTable` with alloc/free/get/clear, 64 slots
  - Message types: re-exports `kernel::msg::Message`
  - 35 tests: IPC error handling, grant lifecycle, endpoint validation

- [ ] **13.3 — Process lifecycle (PM protocol)**
  - `fork`: send PM fork request, receive child endpoint
  - `exit`: send PM exit, cleanup
  - `waitpid`: poll PM for child exit
  - `exec`: send PM exec with binary path + arguments
  - `getpid` / `getppid`
  - Tests: fork + exit + waitpid roundtrip via mock PM

- [ ] **13.4 — File I/O (VFS protocol)**
  - `open`: send VFS open request, receive fd
  - `read` / `write`: VFS read/write with grant-based buffers
  - `close`: VFS close
  - `lseek`: VFS seek
  - `stat` / `fstat`: VFS stat
  - `readdir`: VFS getdents
  - `mount` / `umount`: VFS mount
  - `ioctl`: device control via VFS
  - `select` / `poll`: VFS select
  - Tests: open/read/write/close pipe roundtrip via mock VFS

- [ ] **13.5 — Memory management (VM protocol)**
  - `mmap` / `munmap`: VM remap/unmap
  - `brk` / `sbrk`: heap expansion via VM
  - `mmap` with file backing (VFS + VM)
  - Shared memory (`shmget`/`shmat` via IPC server)
  - Tests: allocate, map, unmap, heap grow

- [ ] **13.6 — Time and signals (CLOCK + PM protocols)**
  - `clock_gettime`: CLOCK server request
  - `nanosleep`: timer via CLOCK
  - `signal` / `sigaction`: PM signal handlers
  - `sigprocmask`: PM signal mask
  - `kill`: PM signal send
  - `alarm` / `setitimer`: timer-based signals
  - Tests: time monotonicity, signal delivery

- [ ] **13.7 — Networking (LWIP protocol)**
  - `socket`: create endpoint via LWIP
  - `bind` / `listen` / `accept`: server socket
  - `connect`: client socket
  - `send` / `recv`: data transfer
  - `getsockopt` / `setsockopt`: socket options
  - Tests: loopback connect/send/recv

- [ ] **13.8 — Minimal `libc` for FFI**
  - Thin wrappers over `minix-std` with C ABI
  - `open`, `read`, `write`, `close`, `lseek`
  - `fork`, `exit`, `waitpid`, `execve`
  - `mmap`, `munmap`, `brk`
  - `clock_gettime`, `nanosleep`
  - `sigaction`, `kill`, `sigprocmask`
  - `getpid`, `getuid`, `getgid`
  - Tests: each function called from Rust `extern "C"` wrappers

- [ ] **13.9 — `crates/minix-util` utility crate**
  - Device manager client (DEVMAN protocol helpers)
  - Block device I/O client
  - Character device I/O client
  - Data store client (DS publish/retrieve helpers)
  - Tests: each client against the corresponding server mock

---

## Phase 14: Userland Commands

**Goal**: Port userland commands. These are pure C with no kernel dependencies beyond libc.

### Priority 1 — Boot critical (need to boot the system)

- [ ] **14.1** — `bin/cat` (`.refs/minix-3.3.0/bin/cat/`)
  - Reads files specified as args (or stdin if none), writes to stdout
  - 8192-byte buffer, handles errors per-file
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.2** — `bin/cp` (`.refs/minix-3.3.0/bin/cp/`)
  - Copies source file to destination via open/read/write loop with 8192-byte buffer
  - Creates destination with O_WRONLY | O_CREAT | O_TRUNC, mode 0644
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.3** — `bin/rm` (`.refs/minix-3.3.0/bin/rm/`)
  - Removes files via `fs::unlink()`, reports error per path
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.4** — `bin/mkdir` (`.refs/minix-3.3.0/bin/mkdir/`)
  - Creates directories via `fs::mkdir()` with mode 0755
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.5** — `bin/ln` (`.refs/minix-3.3.0/bin/ln/`)
  - Creates hard links via `fs::link()`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.6** — `bin/chmod` (`.refs/minix-3.3.0/bin/chmod/`)
  - Changes file mode via `fs::chmod()`, parses octal mode from args
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.7** — `sbin/chown` (`.refs/minix-3.3.0/sbin/chown/`)
  - Changes file owner via `fs::chown()`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.8** — `bin/ls` (`.refs/minix-3.3.0/bin/ls/`)
  - Lists directory contents via `fs::getdents()`, parses dirent structs
  - Filters `.` and `..`, sorts alphabetically, 2-column layout
  - `DirEntry` parser with full dirent field parsing
  - Tests: Compare output against reference C version; argument parsing; error handling; edge cases
- [ ] **14.9** — `bin/echo` (`.refs/minix-3.3.0/bin/echo/`)
  - Joins args with spaces, appends newline, writes to stdout
  - Tests: Compare output against reference C version; argument parsing; error handling; edge cases
- [ ] **14.10** — `bin/sh` — shell (`.refs/minix-3.3.0/bin/sh/`)
  - Minimal shell: line input with editing, split_line parser, PATH lookup,
    built-in cd/exit, fork+exec+waitpid for external commands
  - 6 tests: split_line, search_path
- [ ] **14.11** — `bin/sync` (`.refs/minix-3.3.0/bin/sync/`)
  - Flushes filesystem buffers via `fs::sync()`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.12** — `sbin/init` (`.refs/minix-3.3.0/sbin/init/`)
  - First userspace process: forks /bin/sh, reaps zombies, respawns shell on exit
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.13** — `sbin/mknod` (`.refs/minix-3.3.0/sbin/mknod/`)
  - Creates device nodes via `fs::mknod()` (new minix-std wrapper)
  - Parses type (b/c), major, minor from args
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.14** — `sbin/fsck` (`.refs/minix-3.3.0/sbin/fsck/`)
  - Minimal fsck: reads superblock, validates MFS magic number at offset 0x218
  - 2 tests
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.15** — `sbin/reboot` (`.refs/minix-3.3.0/sbin/reboot/`)
  - Reboots the system via `process::reboot()` (new minix-std wrapper)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.16** — `sbin/shutdown` (`.refs/minix-3.3.0/sbin/shutdown/`)
  - Halts the system via `process::halt()` (new minix-std wrapper)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases

---

## Phase 14.B — First Boot to Userspace

**Goal**: Connect all the existing pieces — kernel, system servers, drivers, and
userland — into a working system that boots to a `#` shell prompt on the serial
console. Currently `kmain()` prints "Hello MINIX!" and enters an HLT loop.

### Tasks

- [ ] **14.B.1 — Wire kmain through full kernel init**
  - After existing boot steps (BSS, serial, GDT/IDT/FPU, proc_init, PIT, PIC):
  - Added `kernel::sched::system::system_init()` — populates kernel call dispatch
    table with handlers for all ~40 syscalls (fork, exec, kill, etc.)
  - Added `kernel::interrupt::reset_irq_state()` — clears IRQ handler table,
    active IDs, and use mask to a clean initial state
  - Added `kernel::clock::set_system_hz(100)` — sets system timer frequency
    to match the PIT (programmed at 100 Hz)
  - Replaced heartbeat HLT loop with cleaner idle loop with comments marking
    where future clock tick, interrupt dispatch, and process scheduling go
  - Added boot message: `[kernel] init complete, entering idle loop`
  - **Deferred**: `setup_syscall_msrs()` — requires a `syscall` entry function
    (naked asm handler). Will be added when the first userspace process is
    created (14.B.2/14.B.3), since the MSR must point to the kernel's real
    syscall dispatch code
  - Verified: kernel compiles cleanly, reaches idle loop without panic

- [ ] **14.B.2 — Boot image and process creation**
  - Added `BootImage` struct and `BOOT_IMAGE` static array matching C `image[]`
  - Implemented `boot_create_procs()`: sets name, endpoint, privilege, priority
  - Kernel tasks, RS, VM get privileges; others inhibited until RS setup
  - Added privilege/scheduling constants to `config.rs`
  - Fixed `proc_addr()` overflow bug for negative process numbers
  - Fixed `NR_BOOT_PROCS` to use formula (was hardcoded 17)
  - Wired into kmain: `[boot] creating boot processes...`
  - 11 tests covering all boot image properties and privilege assignment

- [ ] **14.B.3 — Kernel main message loop**
  - Created `crates/kernel/src/loop.rs` with `kernel_main_loop()`:
    - Processes pending timer ticks via `clock::tick()` then `tmrs_exptimers()`
    - Manages global timer queue (`TIMER_QUEUE`) with `set_timer()`/`cancel_timer()`
    - Idles with HLT when no work is pending
    - Heartbeat dot every 100 ticks (was in kmain, now in main loop)
    - Placeholder comments for future kernel call dispatch and process scheduling
  - Moved `TICK_COUNT` from kernel-boot to `kernel::clock::TICK_COUNT` (shared
    between timer interrupt handler and main loop)
  - Updated timer handler to call `kernel::clock::tick()` directly (advances
    monotonic/realtime clocks on each interrupt)
  - kmain now delegates to `kernel::r#loop::kernel_main_loop()` after init
  - `set_timer()` and `cancel_timer()` public wrappers for timer queue access
  - All unsafe operations properly wrapped (Rust 2024)
  - 35 unit + 96 integration tests pass, clippy clean

- [ ] **14.B.4 — Userspace process startup**
  - Fixed kernel stack allocation (`alloc_kernel_pages`): replaced stub with
    boot-time static pool allocator (16 stacks × 16 KB = 256 KB)
  - Created `kernel::tasks` module with kernel task entry point functions:
    `idle_task()`, `clock_task()`, `sys_task()`, `hw_task()`, `asyncm_task()`
  - Created `boot_proc::boot_setup_process_stacks()` in arch-x86_64:
    allocates kernel stacks and sets up StackFrame (CS/SS/PSW/SP/RIP) for
    each boot process — ring 0 selectors for kernel tasks, ring 3 for user
  - Created `asm::syscall_entry()`: naked asm handler for `syscall`/`sysretq`
    that saves registers, dispatches through `syscall_handler_c()` →
    `arch_syscall::syscall_dispatch()`, restores, and returns
  - Wired `setup_syscall_msrs()` in kmain with IA32_STAR, IA32_LSTAR, IA32_FMASK
  - Enabled `EFER.SCE` (Syscall Enable) bit
  - Replaced HLT loop in kmain with `restore()` → IDLE task; IDLE task now
    processes pending timer ticks and HLTs (same timer behavior, proper
    process switching mechanism)
  - 11 new tests: kernel task entry points, selector values, RFLAGS,
    boot stack pool allocation/exhaustion
  - All unsafe operations use explicit `unsafe {}` blocks (Rust 2024)

- [ ] **14.B.5 — initramfs/ramdisk with binaries**
  - Created `tools/mkinitramfs.rs` — builds all userland binaries for the
    x86_64-pc-minix target and creates a CPIO newc archive at
    `target/initramfs.cpio` with 14 boot-critical binaries, 4 directories
    (/, /bin, /sbin, /dev), 4 device nodes (/dev/tty00, /dev/tty01,
    /dev/null, /dev/console), and generates `target/initramfs_data.rs`
    with the embedded bytes
  - Modified `tools/mkboot.rs` to invoke mkinitramfs after kernel build
  - Created `kernel::initramfs` module with CPIO newc parser (`CpioIter`,
    `CpioEntry`), `find_initramfs_file()`, and `initramfs_data()` accessor
  - Updated `tools/minix-raw.ld` to add `.initramfs` section with
    `__initramfs_start`/`__initramfs_end` symbols
  - 7 unit tests: CPIO parsing roundtrip, directory/device entries,
    invalid magic, file lookup, pad4 alignment
  - All unsafe operations use explicit `unsafe {}` blocks (Rust 2024)

- [ ] **14.B.6 — Server fault tolerance**
  - PM `do_exit()`: added RS notification path — when a process exits whose
    parent is RS, `notify_rs_on_exit()` stores the notification in global
    state that RS can consume via `take_rs_exit_notification()`
  - RS `detect_sigchld()`: implemented — checks PM's exit notification queue
    and scans the RPROC table for terminated services
  - RS `do_restart()`: enhanced with documentation of the fork/exec restart
    flow and restart budget tracking up to `RESTART_MAX`
  - RS `rs_main_iteration()`: main loop iteration that detects crashed
    services and triggers automatic restarts
  - RS `rs_register_boot_services()`: registers all boot-time system servers
    (PM, VFS, SCHED, DS, VM, TTY, MFS, PFS) with RS for crash monitoring
  - Init: improved orphan reaping — `waitpid(-1, 0)` loop reaps all zombie
    children (not just the shell), exits on error to retry fork
  - Clippy clean across workspace

- [ ] **14.B.7 — ELF64 binary loader**
  - Created `crates/kernel/src/elf.rs` (419 lines) with full ELF64 parsing and loading:
  - `Elf64Ehdr` / `Elf64Phdr` — `#[repr(C)]` structs matching x86_64 ELF format
  - `parse_elf_header()` — validates ELF magic, 64-bit, little-endian, ET_EXEC,
    EM_X86_64, and program header entry size
  - `load_elf()` — iterates PT_LOAD segments, copies file data to virtual addresses,
    zero-fills BSS (memsz - filesz), tracks base/top address range
  - `setup_user_stack()` — builds standard ABI stack layout (argc, argv ptrs, envp)
    with 16-byte RSP alignment. Writes strings at top of stack area, aligned down.
  - Constants: `PT_NULL`, `PT_LOAD`, `PT_DYNAMIC`, `PT_INTERP`, `PT_NOTE`, `PT_PHDR`,
    `PT_GNU_STACK`, `PF_X`, `PF_W`, `PF_R`, `ET_EXEC`, `EM_X86_64`, `ELF_MAGIC`
  - 6 unit tests: magic, too-small data, bad magic, 32-bit rejection, big-endian
    rejection, parse valid header, stack setup (single arg, multiple args)
  - Added `pub mod elf;` to `crates/kernel/src/lib.rs`

- [ ] **14.B.8 — Init loading and userspace execution**
  - **`crates/kernel-boot/src/boot_init.rs`** (NEW, 75 lines):
    - `load_and_prepare_init()` — finds `/sbin/init` in initramfs, validates ELF64
      header, loads ELF segments to their virtual addresses, allocates user stack
      (64 KB, initially at `0x3FF00000` but moved to `0x0FE00000` — see bug below),
      writes stack layout with `/sbin/init` argv[0], sets up `Proc::p_reg` StackFrame
      for ring-3 execution (CS=0x1B, SS=0x23, PSW=0x0202, RDI=user_rsp for argc,
      PC=entry point, SP=kernel_stack via swapgs)
  - **`crates/kernel-boot/src/main.rs` kmain updates**:
    - **GDT**: Added user code (0x1B, DPL=3, L=1) and user data (0x23, DPL=3) descriptors
    - **Page tables**: Set User bit on page table entries (0x07/0x87 instead of 0x03/0x83)
      so user-mode code can access mapped memory; TLB flush after setup
    - **kmain flow**: init loading → register IPC syscalls (46-49) → register basic
      userland syscalls (getpid, write, exit, brk) → register PM server dispatch →
      register exec target callback → set current process to init → set up per-CPU
      GS base (IA32_KERNEL_GS_BASE pointing to CPU_LOCAL_STORAGE) →
      mask IRQs (PIC) → **switch to init via restore() → iretq**
    - IRQs masked but NOT enabled with sti — restored via iretq from user RFLAGS
    - 4 GDT descriptor decode tests + existing tests pass
  - **`crates/arch-x86_64/src/asm.rs`**:
    - `syscall_entry` checks `EXEC_TARGET_RIP` after dispatch — if non-zero,
      clears exec globals, sets R11=0x202 (safe RFLAGS), and `sysretq` to new binary
    - `restore()` uses StackFrame.pc ([rdi+88]) directly (was hardcoded to 0x200000
      requiring a trampoline that overwrote kernel .text — removed)
  - **`crates/kernel/src/initramfs.rs`**: Changed from linker section approach to
    `include_bytes!` via `embed_initramfs` feature; initramfs built before kernel
  - **`crates/kernel-boot/Cargo.toml`**: Added `servers` dependency, `embed_initramfs` feature
  - **`crates/kernel/Cargo.toml`**: Added `embed_initramfs = []` feature
  - **Userland GDT descriptors**: Added to both boot_entry (naked_asm GDT) and
    trampoline.S, enabling ring-3 code execution via iretq/sysretq
  - **Bugs found during QEMU debugging (all fixed)**:
    1. **`IA32_KERNEL_GS_BASE` MSR was `0xC0000109`** (should be `0xC0000102`) —
       `swapgs` read uninitialized MSR → GS base = 0 → `gs:0x0` read garbage from
       physical address 0 (real-mode IVT). Fixed in `cpu_msr.rs`.
    2. **GDT code segment D/B=1 with L=1** — illegal per Intel SDM; QEMU treated
       as CS32 compatibility mode. Changed flags from `0x5F` to `0xAF`.
    3. **User stack at `0x3FF00000`** — outside 256MB RAM (identity-mapped to
       physical `0xFFE00000`). Moved to `0x0FE00000`.
    4. **PM_EXEC_NEW constant mismatch** — minix-std had `PM_BASE + 30` (0x01E)
       but servers/pm.rs uses `PM_BASE + 43` (0x02B). Kernel SUSPEND handler
       checked for 0x02B, so exec silently returned without loading shell.
    5. **SLOT_FREE never cleared** — proc_init sets SLOT_FREE on all slots,
       boot_create_procs never cleared it. Deadlock detection panicked.
    6. **Exec stack at 0x3F000000** — same stack-outside-RAM bug as #3.
    7. **SYS_READ handler missing** — shell used VFS IPC for stdin, VFS has
       no dispatch handler, IPC blocked forever. Added syscall 8 direct read.
    8. **Exec handler hardcoded to INIT_PROC_NR** — used hardcoded endpoint
       instead of the actual caller from the IPC message.
    - All now have test coverage except SYS_READ (needs QEMU serial I/O).

- [ ] **14.B.9 — User-facing syscall handlers for boot-to-shell**
  - Registered in kmain before userspace switch:
  - `getpid` (syscall 0) — returns PID 1 (init)
  - `exit` (syscall 2) — halts CPU with CLI+HLT (no process cleanup yet)
  - `write` (syscall 9) — writes to serial (fd 1=stdout, fd 2=stderr),
    handles `\n` → `\r\n` translation
  - `brk` (syscall 13) — simple bump allocator in 0x3FE00000–0x3FF00000 range
  - Fixed `crates/userland/src/lib.rs` syscall argument ordering for x86_64
    ABI (inlateout for rax, correct register mapping)
  - Added `embed_initramfs` feature gating — initramfs built by `mkinitramfs.rs`
    before kernel build in `mkboot.rs`

---

### Priority 2 — Essential userland

- [ ] **14.17** — `bin/date` (`.refs/minix-3.3.0/bin/date/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.18** — `bin/df` (`.refs/minix-3.3.0/bin/df/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.19** — `bin/hostname` (`.refs/minix-3.3.0/bin/hostname/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.20** — `bin/sleep` (`.refs/minix-3.3.0/bin/sleep/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.21** — `bin/test` (`.refs/minix-3.3.0/bin/test/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.22** — `bin/pwd` (`.refs/minix-3.3.0/bin/pwd/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.23** — `bin/kill` (`.refs/minix-3.3.0/bin/kill/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.24** — `bin/expr` (`.refs/minix-3.3.0/bin/expr/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.25** — `bin/mv` (`.refs/minix-3.3.0/bin/mv/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.26** — `bin/rmdir` (`.refs/minix-3.3.0/bin/rmdir/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.27** — `bin/stty` (`.refs/minix-3.3.0/bin/stty/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.28** — `sbin/ping` (`.refs/minix-3.3.0/sbin/ping/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.29** — `sbin/fsck_ext2fs` (`.refs/minix-3.3.0/sbin/fsck_ext2fs/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.30** — `sbin/newfs_ext2fs` (`.refs/minix-3.3.0/sbin/newfs_ext2fs/`)
  - Tests: Compare output against reference C version; argument parsing; error handling; edge cases

### Priority 3 — NetBSD userland (`.refs/minix-3.3.0/usr.bin/` and `.refs/minix-3.3.0/usr.sbin/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases

- [ ] **14.31** — `usr.bin/make` (`.refs/minix-3.3.0/usr.bin/make/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.32** — `usr.bin/grep` (`.refs/minix-3.3.0/usr.bin/grep/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.33** — `usr.bin/sed` (`.refs/minix-3.3.0/usr.bin/sed/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.34** — `usr.bin/find` (`.refs/minix-3.3.0/usr.bin/find/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.35** — `usr.bin/cut` (`.refs/minix-3.3.0/usr.bin/cut/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.36** — `usr.bin/sort` (`.refs/minix-3.3.0/usr.bin/sort/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.37** — `usr.bin/awk` (`.refs/minix-3.3.0/usr.bin/awk/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.38** — `usr.bin/tar` (`.refs/minix-3.3.0/usr.bin/tar/` or via libarchive)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.39** — `usr.bin/gzip` (`.refs/minix-3.3.0/usr.bin/gzip/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.40** — `usr.bin/bzip2` (`.refs/minix-3.3.0/usr.bin/bzip2/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.41** — `usr.bin/bzip2recover` (`.refs/minix-3.3.0/usr.bin/bzip2recover/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.42** — `usr.bin/unzip` (`.refs/minix-3.3.0/usr.bin/unzip/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.43** — `usr.bin/patch` (`.refs/minix-3.3.0/usr.bin/patch/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.44** — `usr.bin/comm` (`.refs/minix-3.3.0/usr.bin/comm/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.45** — `usr.bin/tr` (`.refs/minix-3.3.0/usr.bin/tr/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.46** — `usr.bin/wc` (`.refs/minix-3.3.0/usr.bin/wc/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.47** — `usr.bin/head` (`.refs/minix-3.3.0/usr.bin/head/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.48** — `usr.bin/tail` (`.refs/minix-3.3.0/usr.bin/tail/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.49** — `usr.bin/uniq` (`.refs/minix-3.3.0/usr.bin/uniq/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.50** — `usr.bin/tee` (`.refs/minix-3.3.0/usr.bin/tee/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.51** — `usr.bin/xargs` (`.refs/minix-3.3.0/usr.bin/xargs/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.52** — `usr.bin/uuencode` / `usr.bin/uudecode` (`.refs/minix-3.3.0/usr.bin/uuencode/`, `.refs/minix-3.3.0/usr.bin/uudecode/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.53** — `usr.bin/cksum` (`.refs/minix-3.3.0/usr.bin/cksum/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.54** — `usr.bin/passwd` (`.refs/minix-3.3.0/usr.bin/passwd/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.55** — `usr.bin/login` (`.refs/minix-3.3.0/usr.bin/login/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.56** — `usr.bin/su` (`.refs/minix-3.3.0/usr.bin/su/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.57** — `usr.bin/who` / `usr.bin/w` / `usr.bin/whoami` (`.refs/minix-3.3.0/usr.bin/who/`, etc.)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.58** — `usr.bin/ps` (`.refs/minix-3.3.0/usr.bin/ps/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.59** — `usr.bin/id` (`.refs/minix-3.3.0/usr.bin/id/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.60** — `usr.bin/which` (`.refs/minix-3.3.0/usr.bin/which/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.61** — `usr.bin/env` (`.refs/minix-3.3.0/usr.bin/env/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.62** — `usr.bin/printenv` (`.refs/minix-3.3.0/usr.bin/printenv/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.63** — `usr.bin/dirname` / `usr.bin/basename` (`.refs/minix-3.3.0/usr.bin/dirname/`, `.refs/minix-3.3.0/usr.bin/basename/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.64** — `usr.bin/mktemp` (`.refs/minix-3.3.0/usr.bin/mktemp/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.65** — `usr.bin/touch` (`.refs/minix-3.3.0/usr.bin/touch/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.66** — `usr.bin/stat` (`.refs/minix-3.3.0/usr.bin/stat/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.67** — `usr.bin/nice` (`.refs/minix-3.3.0/usr.bin/nice/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.68** — `usr.bin/renice` (`.refs/minix-3.3.0/usr.bin/renice/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.69** — `usr.bin/true` / `usr.bin/false` (`.refs/minix-3.3.0/usr.bin/true/`, `.refs/minix-3.3.0/usr.bin/false/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.70** — `usr.bin/cal` (`.refs/minix-3.3.0/usr.bin/cal/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.71** — `usr.bin/man` (`.refs/minix-3.3.0/usr.bin/man/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.72** — `usr.bin/clean` (`.refs/minix-3.3.0/usr.bin/col/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.73** — `usr.bin/colrm` (`.refs/minix-3.3.0/usr.bin/colrm/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.74** — `usr.bin/column` (`.refs/minix-3.3.0/usr.bin/column/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.75** — `usr.bin/indent` (`.refs/minix-3.3.0/usr.bin/indent/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.76** — `usr.bin/crc` (`.refs/minix-3.3.0/usr.bin/crc/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.77** — `usr.bin/look` (`.refs/minix-3.3.0/usr.bin/look/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.78** — `usr.bin/spell` (`.refs/minix-3.3.0/usr.bin/spell/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.79** — `usr.bin/diff` (`.refs/minix-3.3.0/usr.bin/diff/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.80** — additional `usr.bin/*` commands (~65 more): `apropos`, `asa`, `banner`, `cal`, `calendar`, `checknr`, `chpass`, `colcrt`, `csplit`, `ctags`, `deroff`, `du`, `expand`, `finger`, `fold`, `fpr`, `from`, `fsplit`, `ftp`, `genassym`, `getopt`, `hexdump`, `jot`, `lam`, `last`, `ldd`, `leave`, `lock`, `logname`, `lorder`, `m4`, `machine`, `man`, `menuc`, `mesg`, `mkdep`, `mkfifo`, `mkstr`, `msgc`, `nbperf`, `newgrp`, `nl`, `nohup`, `pwhash`, `renice`, `rev`, `sdiff`, `seq`, `shar`, `shlock`, `shuffle`, `soelim`, `split`, `touch`, `tput`, `tsort`, `tty`, `ul`, `uname`, `unexpand`, `unifdef`, `unvis`, `users`, `uuidgen`, `vis`, `wall`, `what`, `whatis`, `whereis`, `whois`, `write`, `xinstall`, `xstr`, `yes`, etc.
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.81** — `usr.sbin/*` commands: `chroot`, `i2cscan`, `installboot`, `link`, `mtree`, `postinstall`, `pwd_mkdb`, `rdate`, `traceroute`, `unlink`, `user`, `vipw`, `vnconfig`, `zic` (all in `.refs/minix-3.3.0/usr.sbin/`)
  - Tests: Compare output against reference C version; argument parsing; error handling; edge cases

### Priority 4 — Minix-specific networking commands (`.refs/minix-3.3.0/minix/commands/`)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases

- [ ] **14.82** — `minix/commands/ifconfig`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.83** — `minix/commands/dhcpd`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.84** — `minix/commands/rarpd`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.85** — `minix/commands/irdpd`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.86** — `minix/commands/host` / `hostaddr`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.87** — `minix/commands/add_route` / `arp` / `pr_routes`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.88** — `minix/commands/tcpd` / `tcpdp` / `tcpstat` / `udpstat`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.89** — `minix/commands/telnet` / `telnetd`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.90** — `minix/commands/rsh` / `rshd` / `rcp`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.91** — `minix/commands/ftp`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.92** — `minix/commands/fetch`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.93** — `minix/commands/traceroute`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.94** — `minix/commands/mail` / `lpd`
  - Tests: Compare output against reference C version; argument parsing; error handling; edge cases

### Priority 5 — Administration & utilities

- [ ] **14.95** — `minix/commands/devmand` (device manager client)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.96** — `minix/commands/setup` (system setup)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.97** — `minix/commands/partition` / `fdisk` / `autopart` / `repartition`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.98** — `minix/commands/cdprobe` / `diskctl` / `ramdisk` / `loadramdisk` / `eject`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.99** — `minix/commands/writeisofs` / `isoread`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.100** — `minix/commands/lspci`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.101** — `minix/commands/i2cscan` (from sbin)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.102** — `minix/commands/cron` / `crontab`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.103** — `minix/commands/syslogd` / `logger`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.104** — `minix/commands/service` / `svclog` / `svrctl`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.105** — `minix/commands/postinstall` / `update` / `update_bootcfg` / `updateboot`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.106** — `minix/commands/sysenv` / `version`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.107** — `minix/commands/lua`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.108** — `minix/commands/mined` (text editor)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.109** — `minix/commands/playwave` / `recwave`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.110** — `minix/commands/dhrystone` / `worldstone` (benchmarks)
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.111** — `minix/commands/screendump` / `readclock` / `loadkeys` / `loadfont`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.112** — `minix/commands/progressbar` / `diff`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.113** — `minix/commands/col` / `colrm` / `column` / `indent` / `crc` / `termcap` / `look` / `spell`
  - Tests: Compare output against C version with identical inputs; command-line argument parsing; error handling; edge cases
- [ ] **14.114** — `minix/commands/` remaining commands: `at`, `atnormalize`, `backup`, `btrace`, `cawf`, `ci`, `cleantmp`, `cmp`, `co`, `compress`, `decomp16`, `devsize`, `dosread`, `format`, `fsck.mfs`, `gcov-pull`, `ifconfig`, `ifdef`, `intr`, `ipcrm`, `ipcs`, `nonamed`, `pkgin_all`, `pkgin_cd`, `pkgin_sets`, `profile`, `remsync`, `rotate`, `slip`, `sprofalyze`, `sprofdiff`, `srccrc`, `swifi`, `synctree`, `time`, `truncate`, `vol`, `zdump`, `zmodem`, etc.
  - Tests: Compare output against reference C version; argument parsing; error handling; edge cases

> Each userland command: Test against the C version with identical inputs, compare outputs.

---

## Phase 15: Live Update (LU) Support

**Goal**: Port the live update framework for seamless server/driver updates.

### Tasks

- [ ] **15.1 — Port `minix/servers/is/` — In-Service Update**
  - Source: `.refs/minix-3.3.0/minix/servers/is/`
  - `main.c`, `dmp.c`, `dmp_ds.c`, `dmp_fs.c`, `dmp_kernel.c`, `dmp_pm.c`, `dmp_rs.c`, `dmp_vm.c`, `glo.h`, `inc.h`, `proto.h`
  - LU coordinator, client, server, dump utilities
  - Tests: Live update state machine transitions; SEF event interception; process cloning for LU

- [ ] **15.2 — Port SEF (System Event Framework)**
  - Source: `.refs/minix-3.3.0/minix/include/minix/sef.h` (already identified in Phase 1)
  - Source: `.refs/minix-3.3.0/minix/lib/libsef/` (SEF library)
  - Event interception: init, ping, LU, signal, fault injection
  - State machine: WORK_FREE → REQUEST_FREE → protocol states
  - Tests: Live update state machine transitions; SEF event interception; process cloning for LU

- [ ] **15.3 — Port Live Update protocol handlers**
  - `RS_LU_PREPARE` message handling
  - State synchronization
  - Process cloning for LU
  - Tests: Live update state machine transitions; SEF event interception; process cloning for LU

---

## Phase 16: Networking Stack

**Goal**: Port the networking infrastructure.

### Tasks

- [ ] **16.1 — Port `minix/net/`**
  - Source: `.refs/minix-3.3.0/minix/net/`
  - Network protocol abstractions, socket interface
  - Tests: Network protocol round-trips; socket operations; route table management

- [ ] **16.2 — Port `sys/net/` — NetBSD networking kernel code**
  - Source: `.refs/minix-3.3.0/sys/net/`
  - TCP/IP, UDP, IP, ARP protocols, route table management
  - Tests: Network protocol round-trips; socket operations; route table management

- [ ] **16.3 — Port `sys/netinet/` — Internet protocols**
  - Source: `.refs/minix-3.3.0/sys/netinet/`
  - TCP, UDP, IP, ICMP implementations
  - Tests: Network protocol round-trips; socket operations; route table management

- [ ] **16.4 — Port `sys/netinet6/` — IPv6**
  - Source: `.refs/minix-3.3.0/sys/netinet6/`
  - Tests: Network protocol round-trips; socket operations; route table management

- [ ] **16.5 — Network drivers** (Phase 11c)
  - Tests: Network protocol round-trips; socket operations; route table management

---

## Phase 17: Tools & Build Infrastructure

**Goal**: Port or rewrite the build tools needed to compile the system.

### Tasks

- [ ] **17.1 — Port `tools/` — Minix build tools**
  - Source: `.refs/minix-3.3.0/tools/`
  - Kernel configuration generator, assembly listing tools, `bumpversion`, `checkoldver`, `checkver`, `checkvers`, kernel module tools, `genassym`
  - Tests: Build tool output matches expected format; linker script produces correct ELF layout

- [ ] **17.2 — Port `releasetools/` — Release engineering**
  - Source: `.refs/minix-3.3.0/releasetools/`
  - `build.sh`, snapshot building, distribution packaging
  - Tests: Build tool output matches expected format; linker script produces correct ELF layout

- [ ] **17.3 — Port Makefile.inc patterns**
  - Source: `.refs/minix-3.3.0/Makefile.inc`
  - NetBSD Makefile macros, `bsd.*.mk` files
  - Tests: Build tool output matches expected format; linker script produces correct ELF layout

- [ ] **17.4 — Set up Rust-based build pipeline**
  - Cargo workspace for all Rust crates
  - C build for libraries still in C (zlib, bzip2, etc.)
  - Cross-compile integration
  - Tests: Build tool output matches expected format; linker script produces correct ELF layout

- [ ] **17.5 — Userland linker script + build pipeline**
  - Created `tools/minix-user.ld` — userland linker script linked at 0x01000000:
    - `.text`, `.rodata`, `.data` (with GOT/GOT.PLT/PLT), `.bss` sections
    - `/DISCARD/` for `.eh_frame`, `.note`, `.comment`
  - `tools/mkboot.rs` reordered: initramfs built **before** kernel build (kernel
    needs `initramfs.cpio` via `include_bytes!`)
  - `mkboot.rs` passes `--features embed_initramfs` to kernel build and uses
    `RUSTFLAGS` with `-Ttools/minix-raw.ld` (moved from `.cargo/config.toml`)
  - `tools/mkinitramfs.rs`: builds userland with `-Ttools/minix-user.ld` linker
    script; links at 0x01000000 (separate from kernel at 0x200000)
  - `.cargo/config.toml` cleaned up — rustflags removed from target config
    (linker script now passed via RUSTFLAGS env var in mkboot.rs)
  - Tests: Build tool output matches expected format; linker script produces correct ELF layout

---

## Phase 18: Documentation & Testing

**Goal**: Complete documentation, testing, and polish.

### Tasks

- [ ] **18.1** — Port man pages (`.refs/minix-3.3.0/minix/man/`, `.refs/minix-3.3.0/docs/`)
  - Tests: Doc tests pass; integration tests for each server; driver mock tests; build-time verification checks
- [ ] **18.2** — Add Rust doc comments to all public interfaces
  - Tests: Doc tests pass; integration tests for each server; driver mock tests; build-time verification checks
- [ ] **18.3** — Write integration tests for each server
  - Tests: Doc tests pass; integration tests for each server; driver mock tests; build-time verification checks
- [ ] **18.4** — Write kernel unit tests
  - Tests: Doc tests pass; integration tests for each server; driver mock tests; build-time verification checks
- [ ] **18.5** — Write driver mock tests
  - Tests: Doc tests pass; integration tests for each server; driver mock tests; build-time verification checks
- [ ] **18.6** — Document the Rust codebase (README, architecture docs, API docs)
  - Tests: Doc tests pass; integration tests for each server; driver mock tests; build-time verification checks
- [ ] **18.7** — Update README and porting status
  - Tests: Doc tests pass; integration tests per server; driver mock tests; build-time verification
- [ ] **18.8 — Static MSR constant verification against Intel SDM**
  - `msr_constants` test now asserts `IA32_KERNEL_GS_BASE == 0xC0000102` with
    Intel SDM Vol 4 Table 2-7 reference comment.
- [ ] **18.9 — Static assertion for user stack address within RAM**
  - `user_stack_within_ram` test in kernel-boot asserts stack end < RAM_TOP
    (0x10000000 for 256MB config) and stack base > kernel end.
  - Same constants used by both `boot_init.rs` and `ipc.rs` exec handler.
- [ ] **18.10 — GDT descriptor runtime verification**
  - `gdt_kernel_code_matches_trampoline` and `gdt_user_code_matches_trampoline`
    verify full 8-byte descriptors have L=1, D/B=0, G=1 with spec references.
  - `gdt_decode_byte6()` corrected to use Intel SDM bit positions.
  - Tests: Doc tests pass; integration tests for each server; driver mock tests
- [ ] **18.11 — Inline assembly operand order consistency check**
  - The `syscall_entry` naked_asm uses Intel syntax (confirmed by `push qword ptr`
    tokens), but LLVM may parse segment-override `mov` instructions with
    reversed operand ordering. Add a build-time or test-time check that
    verifies the generated machine code bytes for `mov gs:0x8, rsp` and
    `mov rsp, gs:0x0` are correct (opcode 89 for store, 8B for load).
  - Tests: Doc tests pass; integration tests for each server; driver mock tests
- [ ] **18.12 — QEMU integration test for register values after restore**
  - The `restore()` function clears all GPRs before iretq. Add a test that
    verifies all registers are zeroed (or set to expected values) after
    restore completes. This requires a QEMU-based integration test that
    captures register state after the first iretq.
  - Approach: Add a test mode to the init trampoline that writes register
    values to the serial port, then verify the output matches expectations.
    See `QEMU_ACK` or custom test harness in `tests/`.
  - Tests: Doc tests pass; integration tests for each server; driver mock tests; build-time verification checks

---

## Validation Milestones

### x86_64 Milestones (primary target)

| Milestone | Description | Target Phase | Status |
|-----------|-------------|-------------|--------|
| M1 | Kernel boots in QEMU x86_64, prints banner | Phase 8 | ❌ |
| M1b | **First userspace process execution (iretq to ring-3)** | **Phase 14.B** | ❌ |
| M2 | Two processes can IPC (x86_64) | Phase 4 | ❌ |
| M3 | Process fork + exec works (x86_64) | Phase 5 | ❌ |
| M7b | **System boots to shell prompt (`# ` on serial)** | **Phase 14.B** | ❌ |
| M4 | MFS filesystem serves files (x86_64) | Phase 9 | ❌ |
| M5 | VFS server routes requests (x86_64) | Phase 10 | ❌ |
| M6 | IDE/Virtio driver reads disk (x86_64) | Phase 11b | ❌ |
| M7 | Complete system boots to shell (x86_64) | Phase 14 | ❌ |
| M8 | Network stack works (x86_64) | Phase 16 | ❌ |
| M9 | Live Update works (x86_64) | Phase 15 | ❌ |
| M10 | All drivers functional (x86_64) | Phase 11 | ❌ |
| M11 | All userland commands functional (x86_64) | Phase 14 | ❌ |
| M12 | 100% feature parity with C Minix (x86_64) | Phase 18 | ❌ |

### RISC-V64 Milestones (bonus)

| Milestone | Description | Target Phase |
|-----------|-------------|-------------|
| M1R | Kernel boots in QEMU `virt`, prints banner | Phase 19 |
| M2R | Two processes can IPC (RISC-V64) | Phase 4 (shared) |
| M3R | Process fork + exec works (RISC-V64) | Phase 5 (shared) |
| M4R | Virtio-blk reads disk (RISC-V64) | Phase 19 |
| M5R | Virtio-net sends/receives (RISC-V64) | Phase 19 |
| M6R | Complete system boots to shell (RISC-V64) | Phase 14 + 19 |

---

## Implementation Order Summary (Critical Path)

```
Phase 0: Project structure & build (x86_64 + RISC-V targets)
Phase 1: Foundation types & ABI
Phase 2: Kernel low-level primitives (x86_64 headers + assembly)
Phase 3: Process table & scheduling
Phase 4: IPC system
Phase 5: System calls
Phase 6: VM server (4-level paging)
Phase 7: Clock & interrupts
Phase 8: x86_64 architecture-specific code (boot, paging, syscalls)
  ──────────────────────────────
  EARLY BOOT TEST: Kernel boots in QEMU, prints "Hello MINIX"
  BASIC TEST: Process table works, basic IPC works
Phase 9: File system drivers (start with MFS)
Phase 10: VFS server
Phase 11: Device drivers (start with simple ones)
Phase 12: System servers (SCHED, RS, PM, DS, IPC, DEVMAN)
Phase 13: Shared libraries
Phase 14: Userland commands
Phase 14.B: First boot to userspace (kmain → syscall init → boot image →
           process spawn → initramfs → shell prompt)
  ──────────────────────────────
  BOOT TO SHELL: System boots from QEMU to `# ` prompt on serial console
      14.B.1 kernel init wiring (todo)
      14.B.2 boot image and process creation (todo)
      14.B.3 kernel main message loop (todo)
      14.B.4 userspace process startup (todo)
      14.B.5 initramfs/ramdisk (todo)
      14.B.6 server fault tolerance (todo)
      14.B.7 ELF64 binary loader (todo)
      14.B.8 init loading and userspace execution (todo)
      14.B.9 user-facing syscall handlers (todo)
Phase 15: Live Update
Phase 16: Networking
Phase 17: Tools & build
Phase 18: Documentation & testing
Phase 19: RISC-V64 (bonus — parallelizable after Phase 8 x86_64 is working)
  ──────────────────────────────
  EARLY BOOT TEST (RISC-V): Kernel boots in QEMU -M virt
  BASIC TEST (RISC-V): Process table works, basic IPC works
```

---

## Risk Assessment

### High Risk
1. **`struct proc` and `struct message` ABI** — these must match byte-for-byte with the C layout. Any field reorder in Rust changes the entire IPC protocol.
2. **Assembly integration** — several hundred lines of x86_64 assembly need to interface correctly with Rust code (calling conventions, register allocation, stack layout).
3. **Multiboot 2 / UEFI boot protocol** — the bootloader-to-kernel interface must be correct or nothing boots.
4. **4-level page table manipulation** — bugs here cause immediate panics that are hard to debug without a serial console. No Minix 3.3.0 x86_64 page table code to reference.
5. **Driver framework** — ~30 drivers with varying levels of complexity; some have hardware-dependent quirks.
6. **x86_64 syscall ABI** — `syscall`/`sysret` has different register usage, stack layout, and error handling vs i386 `int 0x80`. No Minix 3.3.0 equivalent to reference.
7. **Self-referential tests** — tests that only assert constants match themselves (not an external spec) provide false confidence. The `IA32_KERNEL_GS_BASE` bug (`0xC0000109` instead of `0xC0000102`) had a passing test that checked the wrong value. Mitigation: every computed constant or MSR number must link to an Intel SDM table reference, and tests must assert against the spec value (not the code constant) where possible.

### Medium Risk
1. **RISC-V64 bonus** — entirely new architecture with no Minix 3.3.0 source to reference. Requires significant design work.
2. **Library porting** — 45+ C libraries need adaptation; some have complex interdependencies.
3. **Userland command porting** — large surface area; ~140 commands, many interact with each other.
4. **Live Update** — complex state machine with subtle timing requirements.
5. **Networking stack** — large codebase with protocol correctness requirements.

### Low Risk
1. **Userland utilities** — mostly pure C with standard library calls.
2. **Filesystem libraries** — MFS is simple; ext2 is well-understood.
3. **Documentation** — mechanical work.

---

## Rust-Specific Design Decisions

1. **`#![no_std]` for kernel, `#![no_std]` + `alloc` for servers**
   - Kernel has minimal heap; uses pre-allocated arrays

2. **IPC messages use `#[repr(C)]` with exact field ordering**
   - Verified at compile time with `static_assert!(size_of::<T>() == expected)`

3. **Process table as a fixed-size array**
   - `let mut proc: [Proc; NR_TASKS + NR_PROCS]` — same as C

4. **Raw pointers for hardware registers**
   - Memory-mapped I/O uses `*mut u32` with `unsafe` blocks

5. **Error handling: `Result<T, Err>` where possible, `panic!` in kernel**
   - Use `core::panic!` for fatal errors in `no_std` kernel context

6. **No heap allocation in kernel**
   - All data structures use static arrays; `alloc` crate for servers

7. **Traits for driver abstraction**
   - `Driver`, `FileSystem`, `NetworkDriver` traits for polymorphism

8. **Use `core::convert::Infallible` and `core::convert::TryFrom`**
   - Zero-cost type conversions matching C casts

9. **`bitflags!` for flag types**
   - RTS flags, MF flags, capability masks

10. **`static_assert!(size_of::<message>() == 56)`**
    - Ensure ABI stability at compile time

11. **`offset_of!` for assembly constants**
    - Generate field offset constants from Rust structs for use in inline assembly

12. **`const { }` blocks for compile-time validation**
    - Validate struct layouts, constant values at compile time

13. **`kernel::klog` — kernel logging subsystem**
    - Leveled logging macros (`klog::error!`, `klog::warn!`, `klog::info!`,
      `klog::debug!`, `klog::trace!`) with compile-time format string checking
    - Output via polled 16550 UART on COM1 (I/O port `0x3F8`)
    - `#[macro_export]` at crate root as `klog_{level}!`, re-exported through
      the `klog` module for the `klog::info!(...)` calling convention
    - Debug/trace levels compiled out in release builds
    - Available from any crate depending on `kernel` (`fs`, `servers`, etc.)
    - `/\n` automatically expanded to `\r\n` for serial terminal compatibility
    - See `crates/kernel/src/klog.rs` for the implementation
