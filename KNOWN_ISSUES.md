# Known Issues — inventory across x86_64 / RISC-V / AArch64

Status snapshot: 2026-08-12, after the stat-path fix (VFS `do_fstat`,
MFS `fs_stat`/`fs_statvfs`), the server-stub cleanup (VM
`clear_pagefault`/`do_vfs_reply`, clock server incl. CLOCK_SETTIME, tty
chardriver primitives), and the tooling fixes (automated per-exec
region/page-count probe, riscv64/aarch64 kernel-binary collision, MSYS
env mangling). Supersedes the older "open questions" lists in FILEMMAP §6
where they overlap; FILEMMAP/TRAPS keep the detailed write-ups.

> **Execution plan:** [OPEN_ITEMS.md](OPEN_ITEMS.md) is the consolidated
> working plan for every open item in this file (steps, files, tests, exit
> criteria; done items removed). Item numbers cited there refer back to
> the sections below.

Legend: `[all]` affects all arches, `[x86]` / `[riscv]` / `[aarch64]`
are arch-specific, `[env]` is tooling/platform, not kernel.

---

## Cross-arch (`[all]`)

1. **VM block cache is a stub (v2 perf work)** — `do_mapcache` /
   `do_setcache` / `do_clearcache` are no-ops; every page fault allocates a
   private page and reads go straight to VFS/MFS each time. No eviction/LRU
   until the real cache lands. Correctness is fine; this is pure perf.
   (FILEMMAP §6)
2. **MFS read-path I/O amplification ~3–6×** — large sequential reads issue
   ~3–6× more virtio-blk I/Os than the file's block count (cat ≈2.8×,
   mmapfd ≈5.6×, 33 MiB `/bin/bign` pre-fault ≈5.8×). Two candidate fixes
   (make PREFETCH read; move `ONE_SHOT` off the `FULL_DATA_BLOCK` value)
   were tried and reverted. Open: profile MFS's cache/read-ahead against
   the virtio request stream. (FILEMMAP §7)
3. **`mmap(fd)` MAP_SHARED semantics** — no consumer yet (exec + the
   `mmapfd` test binary are MAP_PRIVATE-only). A shared file mapping would
   reintroduce COW for shared file pages, which the current per-process
   private-page design avoids. (FILEMMAP §6)
4. **~~VFS `do_fstat` never copies the `Stat` back~~ — FIXED.** The stat
   path now works end-to-end: VFS `do_stat`/`do_fstat`/`do_lstat` pass the
   caller's buffer + `size_of::<Stat>()` to `req_stat` (was `null_mut`, 0);
   MFS `fs_stat`/`fs_statvfs` were `no_sys` in the dispatch table and are
   now implemented (stadir.c port) with the stat written through the VFS
   grant; `req_statvfs` created a bogus grant at VFS address 0 and now
   grants the user's buffer like `req_stat`; userland `stat()`/`lstat()`
   were added to minix-std. Verified: host tests (Stat/Statvfs layout pins,
   `build_stat`/`estimate_blocks` mapping, message formats) + x86 boot test.
   `mmapfd`'s `lseek(SEEK_END)` workaround is obsolete but harmless.
5. **Kernel-mode fault attribution (TRAPS Phase 6)** — `handle_page_fault`
   attributes by `current_proc()`, which is wrong during a
   cross-address-space `virtual_copy` (CR3 owner vs. executor). The exec
   **pre-fault workaround stays** because of this gap, not a resume bug:
   with it disabled, RISC-V's S-mode forwarding fires but the kernel
   attributes the fault to the copier (e.g. VFS) and VM SIGSEGVs it.
   Removing the pre-fault requires fixing attribution first. (TRAPS.md)
6. **Kernel-mode fault resume beyond exec** — all three arches' kernel-mode
   resume paths are in-commit and boot-verified, but the lazy coverage is
   exec-only (text fetch + pre-faulted regions). General kernel-mode copies
   into lazy user pages (future lazy stack/heap, general `vircopy`) still
   need the machinery exercised. (TRAPS.md)
7. **Server Phase 12/13 stubs** (not arch bugs, but known gaps):
   - `tty` — `sys_safecopyto/from` (SYS_SAFECOPYTO/FROM kernel calls) and
     `chardriver_reply_task`/`chardriver_reply_select` (CDEV_REPLY /
     CDEV_SEL2_REPLY builders) are implemented and wired into `dev_ioctl`
     (TIOCDRAIN deferred reply) and `select_retry`. Remaining gaps: the
     ioctl *arg-data* protocol — VFS copies ioctl args inline in the
     message (`cdev_io` uses `ioc_size(request)`, which is 0 for the tty's
     stub request codes 0..21, and the 32-byte inline area can't carry a
     full `Termios`), and VFS's `cdev_reply` async-reply dispatch is still a
     stub — so termios round-trips still don't move real bytes until the
     codes/protocol are aligned; the server-side timer infra (VTIME
     `settimer`) has no kernel timer API for servers.
   - `devman` main loop is a TODO — needs `start_vtreefs`'s VFS message
     loop, which is itself a TODO. Sized: the C libvtreefs is a full FS
     server (inode/link/mount/path/read/stadir/table ≈ 10 files), the
     port's `libs/vtreefs` has only the inode table, and neither procfs nor
     devman is wired to a boot bin or mounted by VFS — a multi-session port
     (NEXT.md Phase A2).
   - `clock_server` main loop is implemented (real RECEIVE → `dispatch_clock`
     → SENDNB loop, `CLOCK_GETTIME` reads the kernel clock via SYS_TIMES,
     `CLOCK_SETTIME` forwards to SYS_SETTIME); note userland
     `clock_gettime` actually goes through PM, which already works.
   - VM `do_vfs_reply` is implemented for the port's synchronous
     VM→VFS protocol (`vfs_request_sync` blocks in sendrec, so out-of-band
     VM_VFS_REPLY messages are rejected with SUSPEND — C's async PENDING
     table has no counterpart here); `clear_pagefault` now forwards
     VMCTL_CLEAR_PAGEFAULT to the kernel (was a no-op).
8. **Threads (THREADS.md open workstreams)** — `thread_local!` in the minix
   std PAL, `Mutex`/`Condvar` over a futex sleep/wake, per-thread errno,
   C-ABI pthread surface; per-thread sigreturn is deferred (process-level
   signal delivery only). Kernel model (1:1, fork/exec/exit group sweep)
   is done and boot-verified.
9. **Fork-from-worker-thread ambiguity — DONE (2026-08-18).** The forking
   tid now flows through the whole fork protocol (userland `fork()` →
   PM_FORK → VM_FORK → SYS_FORK; new `SYS_thread_self` = 64 returns the
   caller's `p_tid`), so the kernel's `do_fork_handler` copies the exact
   forking thread's frame via `find_thread_by_tid` instead of the
   best-effort scan (which stays only as a fallback). Verified by
   `/bin/forkthread` (fork-from-worker, child inherits the worker's tid)
   on x86. (OPEN_ITEMS Phase F2)
10. **Userland `mmap(fd)` has no real consumer** — `mmapfd` is a test
    binary; exec remains the production user. (FILEMMAP §6)

---

## x86_64 (`[x86]`)

- **Piped serial input loses bytes under bursts (the x86 analog of the
  RISC-V issue).** QEMU's chardev pushes a pipe write as one burst; the
  16550 RX FIFO holds only 16 bytes, so a single command line longer than
  that overruns the FIFO and drops bytes mid-word (observed:
  `chmod 4755 /bin/sugid` → `chmod 4755 /bingid`), and back-to-back
  lines are worse. The serial ISR (trigger 14) + syscall-entry drain +
  timer tick can't keep up with a burst. Interactive typing is fine
  (human speed ≪ FIFO drain rate); only scripted piped input is affected.
  Workaround for probes: write input one byte at a time with a ~3 ms gap
  (`su_setuid_probe.py` does this) — no drops observed. A guest-side fix
  would need the UART RX path to drain more aggressively (or QEMU-side
  pacing); not scheduled.
- **Nothing outstanding in-tree.** The repeated-exec leak cascade (fdref
  self-reference, VM self-map VA march, exec-addrspace reclaim, munmap
  walk) is fixed: `exec_loop_mem.py 400` is flat (leak 0.0 KiB/exec), and
  the 100× degradation no longer reproduces. x86 is the best-tested port.
- `[env]` **QEMU SYSRETQ SS.RPL quirk** — QEMU loads SS with the STAR-MSR
  selector without setting RPL=3; the timer ISR's pop-and-rebuild of the
  IRET frame works around it. Platform limitation, workaround in place.
  (bare-metal-debug skill)

---

## RISC-V (`[riscv]`)

1. **Piped serial input can lose bytes under instant `-qmp`/`-s` bursts** —
   the SIE fix (Phase E) landed: `trap_asm.rs` re-enables `SIE` during
   U-mode trap handling (nested ticks skip accounting via the `SPP=1`
   check; the trap-exit path re-masks `SIE` so a nested trap can't fault
   the restore code; `ser_input` ops and the UART drain loops are masked;
   `poll_console` reads the 16550 MMIO first instead of per-byte SBI
   ecalls). Result: plain 10×`hello` piped bursts 10/10 (solid), `-s`
   improved (5/10 → 7-10/10), `-qmp` still 0/10. Two distinct causes:
   (a) `-s`/`-qmp` bursts — QEMU dumps the pipe chunk into the 16-byte
   16550 RX FIFO faster than the guest drains (the x86 analog is
   KNOWN_ISSUES `[x86]`); (b) `-qmp` alone also delivers only 1–2 bytes
   of each pipe write to the FIFO (QEMU `-trace` shows the FIFO empty
   after the first pop — the rest never arrive; `-monitor none` does not
   change it). Both are QEMU-side; probes must pace input (~3 ms/byte).
2. `[env]` **Windows 4 KiB pipe-buffer TX stall** — if a harness captures
   QEMU stdout without draining, the 16550 TX stalls and a blocking
   `putchar` busy-wait looks like a boot hang. Always drain stdout. (RISCV.md)
3. ~~`[env]` **`just test-boot-riscv64` overwrites the normal kernel
   binary**~~ — **FIXED**: each variant is a distinct cargo bin target
   (`kernel-boot-riscv64-{boot,test}` write their own output path, never
   the normal `kernel-boot-riscv64`), so a later `just build-riscv64`
   always relinks the normal kernel (KNOWN_ISSUES tooling notes).
4. **`[toolchain]` virtio-blk reads fail after the 2026-09-17 fork rebase**
   — RISC-V only; x86 and aarch64 are green. `mount_root()` returns null
   because MFS's `req_readsuper` gets EINVAL: the virtio_blk server's read
   of block 0 times out and replies EIO (-5), leaving MFS's block cache
   zeroed (`read_super` sees `magic=0 blk=0`), so VFS never mounts root
   (boot suite: 3 failures, all downstream of this). Not the upstream
   delta: the failure is byte-identical before and after rebasing onto
   c999cef531e. `VBLKDBG probe OK` shows MMIO works and the FEATURES_OK
   handshake completes (the probe validates magic/version/device-id), then
   `read failed err=Unknown` — the status byte is still at its pre-submit
   `0xFF` sentinel, so the device never completes the request. The
   transport programs `q.paddr = ring + phys_delta()` and
   `vd.addr = va + phys_delta()` with the kernel's per-process
   `s_phys_delta = phys_code_base - code_start` (`GET_PHYS_DELTA`);
   hypothesis: that text-derived delta does not hold for the driver's
   `.bss` queue rings/scratch on riscv (layout-dependent, so a compiler
   bump can shift it). Un-instrumented it presents as a silent hang:
   `mount_root()` null → unchecked `mount_devman(null)` → the null+12 load
   fault at 0xc is unmappable → VM livelocks in `sys_vmctl_memreq_get`
   (3.1M traps in 46 s, no console output). Watch it with the
   `VFSDBG`/`MFSDBG`/`BDEVDBG`/`VBLKDBG` markers in `crates/{servers,fs}`
   (bare-metal-debug skill).

---

## AArch64 (`[aarch64]`)

1. **Fork is a proper COW fork (was deep copy).** x86/riscv fork by clearing
   the child's write bit and sharing frames read-only with PhysBlock
   refcounts; aarch64 now does the same, with the access encoding expressed
   through HAL helpers (`pte_is_writable`/`pte_set_writable`/`pte_is_user`)
   because AP[2:1] is a 2-bit field, not a single RW bit. The child's
   owned leaves map the parent's frame with AP = read-only; the parent's
   PTEs are untouched; the shared low-GB alias leaves stay verbatim
   (`alloc::is_alias_frame` — never copied, never COW'd). VM's
   `cow_setup_fork` + the COW message-buffer prefault are active. Verified
   by `/bin/forktest` (fork + write isolation) on all three arches and the
   flat exec loop. (`crates/arch-aarch64/src/fork.rs`, `crates/servers/src/vm/cow.rs`)
2. **Per-exec leak: 0** — the exec loop is flat at leak 0.0 KiB/exec at
   256M/1G/4G.
3. **Kernel-range fault gate — FIXED (D3, 2026-08-17).** aarch64's
   `MAX_USER_ADDRESS` used to cover the whole TTBR0 range (2^44 - 1), so a
   fault at a kernel-range VA (e.g. kernel code at 0x40000000) passed the
   address-based user check and the EL1 handler would eret-retry it forever.
   The ceiling is now the kernel's identity-map base (`kern_vaddr()` =
   0x40000000) — aarch64 user space is only the low 1 GiB below it — so
   kernel-range faults are fatal (halt) instead. VM's temporary
   self-mapping range moved to a dedicated gap below the heap (the old
   "just below the user top" spot now lands on the mmap base). Host test
   `test_user_va_ceiling_below_kernel_window` pins the ceiling; verified
   `just test-boot-aarch64` ALL TESTS PASSED + normal boot to shell.
   (`crates/arch-aarch64/src/vmparam.rs`, `crates/arch-aarch64/src/hal.rs`,
   `crates/servers/src/vm/mod.rs`)
4. **Piped-input verification — DONE (H2, 2026-08-18).** aarch64 piped
   input re-verified with a prompt-wait feed (never before the shell is
   reading): plain 10/10, `-s` 10/10, `-qmp` 8/10. The current kernel
   reaches the shell prompt at ~2 s (the 25–30 s estimate was stale), so
   a partial count is a real delivery drop, not a boot-timing artifact.
   The `-qmp` drops are the QEMU chardev artifact (see `[riscv]` #1).
   Matrix recipe: `tools/piped_matrix.py <arch> <mode>`.
5. **Least-tested port** — standing gotchas (aarch64-debugging skill):
   re-derive every probe address after a rebuild (symbols move), verify the
   embedded initramfs matches the on-disk binaries before suspecting the
   kernel, and check for silent HAL stubs that fail kernel calls
   (`Not implemented`/`TODO` in `crates/arch-aarch64/src/hal.rs` — none
   remain today, but the pattern is the port's classic trap).
6. **Per-process table layout is dual** — boot procs carry PUD[1] as a PMD
   table of 2 MiB blocks; exec'd processes carry a 1 GiB block + the
   low-GB alias PMD. Any generic 4-level walk must handle both, and the
   low-GB alias window must come from the cached kernel geometry
   (`VM_PAGING_MEMINFO`) — the exec-2 hang was exactly this class of bug.
   (aarch64-debugging skill gotcha 9/10)

---

## Testing / tooling notes (`[all]`)

- `exec_loop_mem.py` now takes an `ARCH` argument (x86 | riscv64 | aarch64)
  with per-arch QEMU invocation; aarch64/riscv64 must not use
  `-monitor none` (it drops UART input bytes there).
- `/bin/forktest` (userland bin, `crates/userland/src/bin/forktest.rs`) is
  the fork + COW isolation test: a 4 KiB writable `.data` page is filled,
  forked, and both sides write disjoint patterns; the parent verifies the
  child's write did not land in its view (COW private copy) and the child
  verifies its own write. It runs as a normal command (shell fork → exec);
  for image injection use `MINIXFS_EXTRA=/bin/forktest=...`.
- Two cross-arch bugs found while landing the aarch64 COW fork (fixed):
  (1) `do_vfs_mmap` removed a whole overlapping region when a later
  PT_LOAD segment shared its rounded-up tail page (data memsz spanning the
  bss start page) — it now trims the old region instead, so the dropped
  `.data` pages keep a region; (2) VM `sys_kill` used `send_sig` (records
  the bit, never notifies PM) so a fault with no matching region left the
  process running and re-faulting forever — it now uses `cause_sig`, which
  sets RTS_SIGNALED and notifies the signal manager.
- The per-exec region/page-count check is now automated: VM `VMIW_REGION`
  (target 0) returns the VM-wide region count + total backing pages, the
  shell has a `regions` builtin for it, and `exec_loop_mem.py` samples both
  `memstat` and `regions` every K execs and asserts both stay flat
  (< 4 regions/exec, < 16 pages/exec, < 16 pages/exec leak).
- `[env]` `mkboot`/QEMU process lock (LNK1104) — `taskkill` the stale
  qemu-system-* processes before re-running builds.
- `[env]` MSYS mangles POSIX-style env values (`MINIXFS_EXTRA=dest=...`);
  the `mkfs-*` recipes now set `MSYS2_ENV_CONV_EXCL=MINIXFS_EXTRA` so
  `mkfs.exe` sees the value verbatim.
- `[env]` `just test-boot-riscv64`/`test-qemu-riscv64` (and the aarch64
  equivalents) overwrote the normal kernel binary via the shared cargo
  output path, so a later `just build-riscv64` could report "Finished"
  without relinking. The variants are now **distinct cargo bin targets**
  (`crates/kernel-boot/Cargo.toml`: `kernel-boot-riscv64-{boot,test}`,
  `kernel-boot-aarch64-{boot,test}`) that write their own output paths;
  x86 uses the same scheme via mkboot output stems (`kernel-test.bin` +
  `kernel-test-trampoline.elf`, `kernel-boot.bin` +
  `kernel-boot-trampoline.elf`; the normal build keeps `kernel.bin` +
  `trampoline.elf`). No variant build ever touches the normal kernel
  paths, so `rm`/`cp` shims and `cargo clean -p kernel-boot` are gone.
