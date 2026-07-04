# Test Coverage Analysis

Comprehensive analysis of all test domains in the MINIX Rust port, generated July 2026.

---

## 1. Three Test Execution Domains

| Domain | Runner | Environment | Scope |
|--------|--------|-------------|-------|
| **Host `cargo test`** | `#[test]` in `#[cfg(test)]` | Windows/Linux host | Pure logic, no HW access |
| **QEMU kernel integration** | `test_runner.rs` + `kernel::tests::run_all()` | x86_64 QEMU (ring 0) | Kernel logic with HW (serial, pagetables, timers, interrupts) |
| **QEMU userspace boot** | `kmain -> boot_init -> sysretq` | x86_64 QEMU (ring 3) | Process loading, syscalls, multi-process scheduling |

---

## 2. Bare-Metal QEMU Integration Tests (`just test-qemu`)

Located in `crates/kernel-boot/src/test_runner.rs` (34 test functions) +
`crates/kernel/src/tests.rs` (19 test functions, run as Phase H).

### Phases A–M

| Phase | Tests | What They Prove |
|-------|-------|-----------------|
| **A** (5) | `boot_cr3`, `boot_pml4_entries`, `identity_map_range`, `kernel_high_map`, `serial_output` | Boot CR3 valid, PML4 entries correct, identity map works, COM1 serial output |
| **B** (3) | `pt_walk_boot`, `pt_map_unmap`, `pt_mapkernel` | Page table walk API, map/unmap roundtrip, kernel high mapping |
| **C** (2) | `alloc_free_page`, `alloc_contig` | Physical allocator: single page and contiguous allocation |
| **D** (2) | `vm_alloc_free`, `vm_alloc_multi` | VM allocator: page alloc/free, sequential multi-page |
| **F** (5) | `proc_addr_valid`, `proc_addr_invalid`, `endpoint_lookup`, `is_empty_proc`, `is_kernel_vs_user` | Process table: addressing, endpoint lookup, empty/kernel checks |
| **G** (2) | `mini_notify_when_receiving`, `mini_send_queues_when_not_receiving` | IPC: mini_send and mini_notify |
| **H** (19) | `kernel::tests::run_all()` (see §2.1) | Kernel unit tests running in QEMU |
| **I** (3) | `grant_direct_valid`, `grant_indirect`, `grant_invalid_id` | Grant system: direct, indirect, invalid ID handling |
| **J** (4) | `syscall_getpid`, `syscall_write`, `syscall_brk`, `syscall_exit` | Syscall dispatch: getpid, write return value, brk allocator semantics, exit (EDONTREPLY + SLOT_FREE) |
| **K** (3) | `timer_set_and_expire`, `timer_clear`, `timer_multiple` | Timer queue: set, clear, multiple |
| **L** (2) | `pit_programmed`, `monotonic_advances` | PIT counter programmed at 100 Hz; monotonic clock advances from timer interrupts |
| **M** (1) | `irq_put_and_remove` | Interrupt handling: install/remove handlers |
| **N** (1) | `elf_load_to_phys_pages` | ELF binary loaded into VM-allocated physical pages via identity map; data/BSS readback verification |
| **O** (2) | `rtc_cmos_reads_reasonable_time`, `keyboard_controller_present` | CMOS/RTC registers readable via I/O ports with reasonable time values; PS/2 controller responds to self-test (0x55) |
| **E** (1) | `sysretq_ring3` | **FINALE**: restore() loads RAX from Proc, zeroes RBX/RDX/RSI/RDI/R8-R15, sysretq to ring-3; ring-3 code validates RBX==0 and RAX==0x42 before exiting QEMU |

**Total hardware QEMU tests: 40**

### 2.1 Kernel QEMU-Compatible Tests (`kernel/src/tests.rs`)

These run **inside QEMU** as Phase H, using the same `run()`/`TestCtx` harness:

| Category | Tests | Functions |
|----------|-------|-----------|
| **ELF** (3) | `ehdr_size`, `phdr_size`, `elf_constants` | Struct sizes, ELF magic/machine constants |
| **CPIO** (1) | `cpio_parse_simple` | CPIO archive construction and parsing |
| **IPC** (5) | `mini_send_direct`, `mini_send_queue`, `mini_notify`, `sendrec_direct`, `sendrec_reply_cycle` | Direct delivery, queueing, notify; SENDREC send-half; full SENDREC→reply roundtrip with reversibility |
| **Process Table** (6) | `proc_addr_tasks`, `proc_addr_oob`, `endpoint_encoding`, `endpoint_lookup`, `is_ok_proc_nr`, `is_kernel_nr` | Slot addressing, endpoint roundtrip, bounds checks |
| **Timer** (1) | `tmr_never` | TMR_NEVER == u64::MAX |
| **Scheduler** (4) | `enqueue_dequeue`, `sched_priority`, `sched_round_robin`, `sched_proc_no_time` | Run-queue enqueue/dequeue; priority ordering; round-robin cycling; proc_no_time preemption with NO_QUANTUM and notify_scheduler |
| **Privilege** (2) | `priv_default`, `priv_flags` | Priv struct default fields |
| **Process** (2) | `proc_size`, `proc_ptr_ok` | Proc struct size ≤ 1024, PMAGIC validation |

**Total: 23 QEMU-compatible kernel tests**

---

## 3. Host `cargo test` — Per-Crate Breakdown

### 3.1 `arch-common` (foundation types & ABI) — ~55 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `com.rs` | 19 | All message base constants, notification, bus/PCI, DL, DIO, sysinfo, privctl, vmctl, RS, DS, VFS/PM, VM, IPC, scheduling, USB, devman, TTY, input, cdev, bdev, rtcdev, suspend |
| `consts.rs` | 4 | Process limits (NR_PROCS=256), click constants, file mode bits, special values |
| `devio.rs` | 2 | Port type size, pair sizes |
| `dmap.rs` | 4 | Major numbers, memory minors, ctrlr, full device numbers |
| `endpoint.rs` | 3 | Endpoint encoding roundtrip, negative proc_nr, special endpoints |
| `ipc.rs` | 3 | Message offsets, m1 roundtrip, asynmsg offsets, error codes |
| `ipcconst.rs` | 3 | IPC call constants, status call_to/from roundtrip, status flags |
| `safecopies.rs` | 4 | Grant invalid/valid, CPF flags, CpGrant size, VscpVec size |
| `sys_config.rs` | 4 | FP format, debug lock, KMESS_BUF_SIZE, stack limit |
| `types.rs` | 4 | Primitive sizes, struct sizes, field offsets, constants |
| `vm.rs` | 5 | VM constants, cache flags, struct sizes, VMC_NO_INODE |

### 3.2 `arch-x86_64` (x86_64 architecture-specific) — ~125 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `alloc.rs` | ~20 | MMAP add/merge/cut/totals, alloc single/contig/free, reserve, bitmap overflow, 4GB+ regions |
| `apic.rs` | ~35 | ApicMode enum, LVT NMI detection, version info, register offsets, IOAPIC RTE indexing, PIC IMR port/bit, MSR constants |
| `arch_proc.rs` | 4 | TrapFrame init, PSL_USERSET, zero entry point |
| `arch_syscall.rs` | 4 | SYSCALL_CS, SYSRET_CS, STAR selector roundtrip, SF_MASK |
| `asm.rs` | ~15 | Function signature compilation, ld_dr, rdtsc monotonic, cpuid, STR, SGDT, FX/FPU, CR type checks, HLT/IN/OUT type checks |
| `cpu_msr.rs` | 4 | MSR constants, EFER bits, STAR values, KERNEL_GS_BASE |
| `cpulocals.rs` | ~15 | CpuLocalVars defaults, run_q arrays, idle_proc_ptr, storage init/accessors/setters, NR_SCHED_QUEUES, double init, FPU fields |
| `cpuvar.rs` | 3 | CpuInfo size, CPU roles, default |
| `frame.rs` | 3 | TrapFrame (184), IntrFrame (200), SwitchFrame (56) sizes |
| `hw.rs` | ~20 | Atomic CAS/exchange/add, IDT gate construction (int/trap, DPL, IST, offset, present/not-present, roundtrip), serial constants, TSC, FPU |

### 3.3 `arch-riscv64` (RISC-V64 architecture) — ~24 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `alloc.rs` | 4 | MMAP empty/add, alloc free roundtrip/contig/exhaustion |
| `clint.rs` | 2 | read_time, CLINT constants |
| `cpulocals.rs` | 2 | Storage size, boot storage initialized |
| `mcontext.rs` | 1 | Mcontext size (520) |
| `param.rs` | 4 | Page size, NPTEPG, KERNBASE, round/trunc_page |
| `plic.rs` | 2 | PLIC_BASE, UART_IRQ, IRQ ranges |
| `psl.rs` | 2 | sstatus bits, sie bits, PSL_USERSET |
| `pte.rs` | 5 | PTE size/flags/PPN mask, make_pte, leaf/branch, page table indices, user permissions |
| `sbi.rs` | 2 | Legacy constants, dbcn constants |

### 3.4 `kernel` (core kernel) — ~100 host tests + 18 QEMU tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `clock.rs` | ~18 | TMR_NEVER, timer size, clock accessors, adjtime, timer queue (single/ordered/expiration/not-expired/clrtimer/multiple/partial/remove-head/middle), kernel timer interface, time conversion, set_system_hz |
| `debug.rs` | ~15 | rtsflagstr empty/sending/multiple, miscflagstr empty/FPU, schedulerstr, proc_ptr_ok/null, print_proc/null, itoa, IPC record (increment/stress/top-talkers/reset/clear), mtypename, hook_ipc_clear |
| `elf.rs` | 18 | ehdr/phdr size, parse valid/bad magic/32bit/big-endian/not-exec/wrong-arch/truncated, load_elf valid/no-load-segments, loaded_elf_bounds, constants, phdr_flags, stack_setup single/multiple/too-small/empty-argv |
| `exec.rs` | 2 | exec_setup_new_page_table fails without boot_cr3, BOOT_CR3 initial value |
| `glo.rs` | 12 | Default values, boottime, cpu_hz get/set/OOB, vm_flags, verbose_boot, KInfo/Machine/KMessages/LoadInfo/KRandomness layouts, MinixKernInfo magic, bkl_stats, ipc_call_names |
| `grants.rs` | ~28 | Grant constants/valid/flags/struct_sizes, verify_grant (invalid/negative/OOB/not-used/flags), direct (valid/offset/out-of-range/any/wrong/write/rw), indirect (single-hop/max-depth/wrong), magic (valid/any/wrong/who-from), safecopy (none/zero/from/to/OOB), vsafecopy (zero/no-self/single/multi/fails) |
| `initramfs.rs` | 8 | header_size, cpio_magic, find_file, missing_file, multiple_entries, empty, bad_magic, pad4, trailer |
| `tests.rs` | 18 | (QEMU Phase H — see §2.1) |

### 3.5 `servers` (system servers) — ~90 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `clock_server.rs` | ~18 | ClockTimeSpec ticks conversion (from/as/is-zero/add/sub), clock_id_values, getres, dispatch (getres/gettime/settime/invalid/unknown), timespec_size |
| `devman.rs` | ~18 | init_creates_root, alloc_device_slot, find_device_by_id, add_child/multiple/nested, get/put_device, add_static_info, bind/unbind (wrong-source/nonexistent/sets-state), device_state_transitions, slot_full, root_dev, event_read, msg_i32 |
| `ds.rs` | ~13 | init, publish/retrieve/overwrite/nonexistent, label endpoint, delete/nonexistent, store_full, subscribe (check/exists/overwrite/check-changes/all-types), pattern_match (exact/wildcard/no-anchors), bitmap ops, constants, server_main, unknown source |
| `ipc.rs` | ~32 | Semaphore (create/find/existing/exclusive/no-create/full/invalid-nsems/ctl-getval/setval/getncnt/getpid/getzcnt/invalid/rmid/info/stat/semop/simple/invalid/zero/overflow/identifier/negative/unknown), SHM (create/find/existing/exclusive/no-create/zero/full/ctl-info/shm-info/shm-stat/invalid/rmid/shmdt/empty/unknown), VM stubs, msg helpers |
| `mutex.rs` | 6 | Lock/unlock, try_lock, guard deref_mut, struct, array, try_lock reacquire |

### 3.6 `fs` (filesystem servers) — ~80 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `block_io.rs` | 3 | ram_disk read/write, out_of_bounds, multiblock |
| `ext2/table.rs` | 3 | table_size, dispatch OOB, dispatch no_sys |
| `ext2/utility.rs` | ~10 | conv2/4 native/swap, min_u, no_sys, ansi_strcmp, setbit/setbyte, unsetbit |
| `iso9660/consts.rs` | ~6 | OK, standard_id, super_block, block_size, D_TYPE, errno_values |
| `iso9660/inode.rs` | 5 | init_inode_cache, get_free_dir/ext_attr, create/release dir_record |
| `iso9660/main.rs` | 3 | sef_local_startup, get_work, reply |
| `iso9660/misc.rs` | 3 | fs_sync, fs_flush, fs_new_driver |
| `iso9660/mount.rs` | 3 | fs_readsuper/unmount/mountpoint |
| `iso9660/path.rs` | 2 | get_name, advance no_inode |
| `iso9660/read.rs` | 2 | fs_readwrite, fs_bread |
| `iso9660/stadir.rs` | 3 | fs_stat, fs_statvfs, fs_blockstats |
| `iso9660/super.rs` | 1 | block_read |
| `iso9660/utility.rs` | ~9 | no_sys, do_noop, memcpy_bytes, read/write LE/BE u16/u32, iso_date_to_unix, days_from_ymd |
| `mfs/cache.rs` | 2 | alloc_zone NO_DEV, free_zone NO_DEV |
| `mfs/inode.rs` | 1 | hash_inum |
| `mfs/link.rs` | 4 | fs_link/unlink/rdlink/rename returns EINVAL |
| `mfs/main.rs` | 2 | mfs_init, buffer_cache read from ram_disk |
| `mfs/misc.rs` | 1 | fs_sync |
| `mfs/mount.rs` | 2 | fs_unmount/mountpoint returns EINVAL |
| `mfs/open.rs` | 4 | fs_inhibread/create/mkdir/mknod returns EINVAL/ENOENT |
| `mfs/path.rs` | ~12 | advance (empty/null/dot/root), search_dir (non-dir/empty), fs_lookup (empty/no-null/long), is_name (dot/dotdot), dir_name_match |

### 3.7 `drivers` (device drivers) — ~105 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `bus/i2c.rs` | 15 | Init, reserve (valid/OOB/twice), check_reservation, release, exec (no-process/custom), device new/Default, bus_id, build_key, NR_I2C_DEV, Send |
| `bus/pci.rs` | 8 | Constants, device new, ACL add/check/remove/table_full, bus new, acl new, init |
| `bus/ti1225.rs` | ~10 | Constants, socket/state new, register offsets, ctrl bits, init, detect, power, reset, bridge count/get, card_state default |
| `bus/virtio.rs` | ~12 | Constants, type sizes, vring init, use_desc (readable/writable), chain, descriptor cycle, host_supports, error traits, PCI config addr, default |
| `clock/rtc.rs` | ~12 | BCD conversion, rtc_time zero/constants/struct Send/year_conversion, register bits, CMOS status, signatures |
| `eeprom/cat24c256.rs` | ~12 | Valid addresses, constants, geometry, open/close, read/write (chunk/nopage/large/empty/offset/multiple/boundary), ioctl exec |
| `input/constants.rs` | ~8 | I/O ports, controller commands, status bits, scancode, LED flags/combinations, HID pages, event values/flags, modifier keys, keyboard buffer, KB ack, timing |
| `input/controller.rs` | 5 | I/O ports, cmd0/1, status bits, command constants, LED constants |
| `input/driver.rs` | 7 | Key press/release/ext0, mouse (byte/movement/button), colemak, new, null_callbacks |
| `input/keyboard.rs` | ~14 | Normal key press/release, ext0 prefix/release, ext1 pause/non-pause, modifiers (left/right shift, ctrl, alt), unmapped scancode, colemak |

### 3.8 `userland` (user programs) — ~18 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `lib.rs` | ~18 | echo, cat (ignored), mkdir, rm, ln, chmod, chown, mknod, sync, reboot, fsck, sh, init (ignored), errstr (known/unknown/negative) |

### 3.9 `minix-rt` (MINIX runtime) — 11 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `lib.rs` | 11 | syscall_numbers, syscall0/6 signatures, alignment, brk, sbrk/write/getpid/exit signatures, allocator layout, buf_writer/overflow, allocator Send/Sync |

### 3.10 `minix-libc` (C library compat) — 12 tests

All gated on `target_os = "none"` (compile-only on host).

| Module | Tests | Coverage |
|--------|-------|----------|
| `lib.rs` | 12 | strlen, memset, memcpy, memmove, open/read/write/close/fork/exit/mmap/kill signatures |

### 3.11 `minix-std` (MINIX std lib) — ~30 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `fs.rs` | ~20 | VFS call numbers, open flags, seek constants, file type constants, stat struct layout, message formats (open/read/write/close/lseek/fstat/ioctl/getdents/fsync/truncate), ENOSYS-on-host stubs, signatures |
| `lib.rs` | ~10 | ANY/NONE/SELF constants, proc_nr constants, kernel endpoints, is_ok_endpoint |

### 3.12 `minix-util` (utility library) — ~23 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `bdev.rs` | ~10 | BDEV constants, ENOSYS-on-host stubs (open/close/read/write/ioctl), msg helpers, build_msg, check_result |
| `cdev.rs` | ~10 | CDEV constants, ENOSYS-on-host stubs (open/close/read/write/ioctl/cancel/select), msg helpers, build_msg |
| `devman.rs` | ~3 | DEVMAN constants |

### 3.13 `libs` (shared libraries) — ~14 tests

| Module | Tests | Coverage |
|--------|-------|----------|
| `libminixfs/tests.rs` | 12 | bufhash, buf_pool_init, buf_zeroed, is_clean/locked, get_put_block roundtrip, markdirty/isclean, invalidate, get_block_ino, no_read_prefetch, lru_chain, bufs_in_use, NO_BUF, constants |
| `vtreefs/mod.rs` | 1 | vtreefs_all (monolithic: init, create, lookup, iterate, etc.) |
| `lib.rs` | 1 | it_works (placeholder) |

### 3.14 `net` (networking) — 1 test

| Module | Tests | Coverage |
|--------|-------|----------|
| `lib.rs` | 1 | placeholder |

---

## 4. Summary Totals

| Domain | Count | Notes |
|--------|-------|-------|
| **QEMU integration** (A–O) | **40 tests** | Hardware validation (unchanged) |
| **Kernel QEMU-compatible** (Phase H) | **23 tests** | Pure-logic kernel tests running in QEMU (incl. proc_no_time preemption) |
| **Host — arch-common** | ~55 | Type layouts, ABI constants, IPC message formats |
| **Host — arch-x86_64** | ~125 | Allocator, APIC, paging, IDT, cpulocals, asm ops |
| **Host — arch-riscv64** | ~24 | Allocator, PTE, SBI, PLIC, CLINT, cpulocals |
| **Host — kernel** | ~100 | Clock, debug, ELF, exec, globals, grants, initramfs |
| **Host — servers** | ~90 | Clock server, devman, DS, IPC (sem/shm), mutex |
| **Host — fs** | ~80 | MFS, ISO9660, Ext2, block I/O |
| **Host — drivers** | ~105 | I2C, PCI, TI1225, virtio, RTC, EEPROM, input |
| **Host — userland** | ~18 | Shell commands, errstr |
| **Host — minix-rt** | 11 | Syscall numbers/signatures, allocator |
| **Host — minix-libc** | 12 | Mem ops, signatures (target_os=none) |
| **Host — minix-std** | ~30 | VFS messages, constants, stubs |
| **Host — minix-util** | ~23 | BDEV/CDEV/DEVMAN stubs |
| **Host — libs** | ~14 | libminixfs block cache, VTreeFS |
| **Host — net** | 1 | Placeholder |
| **Host total** | **~690 tests** | All crates combined |
| **Grand total** | **~799 tests** | Host + QEMU (+61 tests from gap-filling) |

---

## 5. Test Quality Assessment

### 5.1 High-Value Tests

| Area | Why High Value |
|------|----------------|
| **QEMU Phases A–L** (30 tests) | Run on real x86_64 QEMU; validate kernel boot, page tables, allocators, IPC, timers, interrupts, and culminate in ring-3 transition |
| **Kernel QEMU Phase H** (18 tests) | Kernel logic running in actual kernel environment |
| **Grant/safecopy** (~28 tests) | Exhaustive: direct, indirect, magic, vsafecopy with edge cases |
| **IPC server semaphores/SHM** (~32 tests) | Complex state machine with create/find/exclusive/delete/stats |
| **DevMan device tree** (~18 tests) | Device state transitions, bind/unbind, slot exhaustion |
| **Timer queue** (12+ tests) | Insertion ordering, expiration, partial, head/middle removal |
| **Physical allocator** (~20 tests) | Bitmap overflow, 4GB+, reserve, exhaustion |
| **Keyboard driver** (~14 tests) | Modifiers, ext0/ext1 sequences, colemak, unmapped |

### 5.2 Medium-Value Tests

| Area | Why Medium |
|------|------------|
| **ELF parser** (18 tests) | Good validation of parsing; stack setup tested in multiple configs |
| **CPIO parser** (8+2 tests) | Good coverage of find/missing/multiple/empty |
| **IDT gate construction** (~15 tests) | Systematic: DPL, IST, offset, present, int vs trap |
| **APIC register modeling** (~35 tests) | Thorough bit-level constant validation |
| **Buffer cache** (12 tests) | LRU order, dirty/clean, invalidation |
| **RTC/CMOS** (~12 tests) | BCD roundtrip, year conversion, register bits |
| **I2C bus** (15 tests) | Reservation lifecycle, exec, edge cases |
| **MFS path operations** (~12 tests) | Empty/null/root/long paths, search_dir edge cases |

### 5.3 Low-Value (but Necessary) Tests

| Area | Why Low | Why Necessary |
|------|---------|---------------|
| **Constant assertions** (~350+) | Self-referential: test asserts what code already says | ABI compatibility — must match C Minix exactly |
| **Signature checks** (~15 tests) | Only verify function types compile | Catches refactoring breakage |
| **ENOSYS stubs** (~10 tests) | Test that stubs return ENOSYS | Ensures host build doesn't crash on MINIX-specific calls |

### 5.4 Previously Addressed Gaps

The following gaps from the original analysis have been filled:

| Gap | Fix | Tests Added |
|-----|-----|-------------|
| **IPC `senrec` (SENDREC)** | Added `test_sendrec_direct` to `kernel/src/tests.rs` | Verifies SENDREC delivers message directly when dst is RECEIVING, overwrites m_source in deliver msg, and blocks sender waiting for reply |
| **Syscall dispatch** | Added `test_syscall_write` and `test_syscall_brk` to `test_runner.rs` Phase J | Write returns correct byte count; brk query/set/out-of-range (ENOMEM) semantics |
| **PIT timer ISR** | Added `test_pit_programmed` and `test_monotonic_advances` to `test_runner.rs` Phase L | PIT counter reads back in valid range for 100 Hz; monotonic clock advances (proves timer interrupts fire) |
| **SENDREC→reply cycle** | Added `test_sendrec_reply_cycle` to `kernel/src/tests.rs` | Full IPC roundtrip: SENDREC request delivery, sender blocks, reply delivery, receiver blocks, roundtrip reversibility |
| **Syscall exit** | Added `test_syscall_exit` to `test_runner.rs` Phase J | Exit returns EDONTREPLY, stores exit status in p_signal_received, sets SLOT_FREE |
| **ELF loading to physical pages** | Added `test_elf_load_to_phys_pages` to `test_runner.rs` Phase N | Minimal ELF built, parsed, loaded into VM-allocated pages via identity map; data/BSS readback verified; entry point validated |
| **Context switch `restore()` + register validation** | Replaced test_sysretq_ring3 with restore()-based version that validates RBX==0 and RAX==0x42 via ring-3 code before QEMU exit | Proves restore() loads RAX from Proc[0], zeroes GPRs, and sysretq delivers correct register state to ring-3 |
| **Driver integration (RTC/CMOS, keyboard)** | Added `test_rtc_cmos_reads_reasonable_time` and `test_keyboard_controller_present` to `test_runner.rs` Phase O | RTC registers readable with reasonable values; PS/2 controller responds to self-test command |

### 5.5 Remaining Notable Gaps

| Area | Gap | Risk |
|------|-----|------|
| **Scheduler preemption (proc_no_time + notify_scheduler)** | Added `test_sched_proc_no_time_preempts` to `kernel/src/tests.rs` | Proves proc_no_time with PREEMPTIBLE sets NO_QUANTUM, dequeues process, pick_proc returns next; all-blocked returns None; round-robin cycling after quantum renewal |
| **VFS↔MFS IPC** | No QEMU or integration test | **High** — M5 |
| **Network** | 1 placeholder test | Phase 16 not started |
| **Live Update** | No tests | Phase 15 not started |
| **RISC-V64 integration** | M1R prints banner; no test suite analogous to Phases A–L | Medium |
| **Driver integration (real hardware)** | RTC/CMOS, PS/2 keyboard controller tested in QEMU; virtio-blk, PCI, ATA, network not tested | **High** — 2 of ~30 drivers tested on real (emulated) HW |
| **Windows host gaps** | Several tests `#[ignore]`'d due to ring-0 / IOPL restrictions | Low (kernels don't test on Windows) |

---

## 6. Test Infrastructure

### 6.1 QEMU Test Architecture

```
crates/kernel-boot/src/test_runner.rs
├── TestCtx { failed: bool }
├── run(name, fn) -> u32        // 0 = pass, 1 = fail
├── serial_putc / serial_puts   // serial output for test results
├── print_hex                   // diagnostic helper
├── qemu_exit_success / qemu_exit_failure  // isa-debug-exit port 0x501
└── run_integration_tests() -> !   // orchestrates all phases

crates/kernel/src/tests.rs
├── TestCtx { failed: bool } (duplicated for kernel crate)
├── run(name, fn) -> u32
├── ser_write / ser_putc       // HAL-based serial (works in-kernel)
└── run_all() -> u32           // invoked from test_runner.rs Phase H
```

### 6.2 Host Test Isolation

| Mechanism | Purpose |
|-----------|---------|
| `#[ignore]` | Tests that need ring-0 or MINIX ABI (syscall instruction, I/O ports) |
| `#[cfg(target_os = "none")]` | Tests that can only compile for the MINIX target (minix-libc mem ops) |
| `TestLockGuard` (devman, ipc servers) | Serializes tests that share global `UnsafeCell` state |
| `static TEST_LOCK` (devman, ipc) | AtomicBool-based test serialization for multi-test modules |

### 6.3 Build / Run Commands

| Command | Effect |
|---------|--------|
| `cargo test` | Run all host unittests (some `#[ignore]`'d on Windows) |
| `just build` | Build kernel + trampoline + initramfs |
| `just run` | Boot full multi-process system in QEMU |
| `just test-qemu` | Run 34 integration tests (A–M) in QEMU, exits via isa-debug-exit |
| `just image` | Build disk image (MBR + stage2 + kernel) |
| `just run-img` | Boot disk image in QEMU (BIOS disk boot path) |

---

## 7. Phases F–L: Planned Component Tests (Not Yet Implemented)

The plan outlines these for `test_runner.rs`:

| Phase | Component | Tests |
|-------|-----------|-------|
| **F** | Process table | `proc_addr`, `endpoint_lookup`, `proc_init` (✅ DONE) |
| **G** | IPC | `mini_send`, `mini_receive`, `mini_notify`, `do_sync_ipc` (✅ partial — SENDREC added, reply cycle added, no full scheduler-mediated cycle) |
| **H** | Scheduler preemption | `proc_no_time` → NO_QUANTUM → dequeue → pick_proc next → all-blocked None → round-robin renewal (✅ DONE) |
| **I** | Grants | `grant_set`, `verify_grant` with `CpGrant` table (✅ DONE) |
| **J** | Syscalls | `getpid`, `write`, `brk`, `exit` dispatch (✅ partial — getpid/write/brk/exit done in QEMU) |
| **K** | Timers | `tmrs_settimer`, `tmrs_exptimers`, `tmrs_clrtimer` (✅ DONE) |
| **L** | PIT/hardware timer | PIT counter readback, monotonic clock advancement (✅ DONE) |
| **M** | Interrupts | `put_irq_handler`, `irq_handle`, `rm_irq_handler` (✅ DONE) |
| **N** | ELF loading to physical pages | Build ELF → parse → alloc pages → load via identity map → readback verify (✅ DONE) |
| **O** | Driver hardware access | RTC/CMOS register readback, PS/2 controller self-test (✅ DONE) |

---

## 8. Running Kernel Unit Tests on QEMU

The `kernel::tests::run_all()` infrastructure (feature = "qemu-tests") makes it
straightforward to port kernel-internal tests to QEMU. Currently 22 tests run
this way. To extend:

1. Write a test function `fn test_my_feature(ctx: &mut TestCtx)` in
   `kernel/src/tests.rs` (or a `qemu_tests` module within each sub-module)
2. Add `total += run("my_feature", test_my_feature);` to `run_all()`
3. The test runs automatically in QEMU as Phase H

Tests from `arch-common`, `arch-x86_64`, and `arch-riscv64` that are pure logic
(no heap, no std) can be ported similarly. Tests from `servers`, `fs`, and
`drivers` that use `alloc` or `Vec` would require a kernel allocator and are
harder to move to QEMU.
