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
3. **`mmap(fd)` MAP_SHARED semantics** — implemented (MAP_SHARED sets
   `VR_SHARED`, the pages are file-cache frames shared between processes) and
   exercised by `mmapfd shared`, which forks and checks that the child's write is
   visible in the parent's view. It is deliberately the *only* place a frame is
   shared for writing: the fork keeps such a page aliased (item 27) and
   `handle_cow_fault` re-enables writability in place instead of copying. What
   is still unbuilt is the *reverse* case — a shared *anonymous* mapping, or a
   file mapping whose cache page is evicted underneath it (item 1's stub cache).
   (FILEMMAP §6)
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

   *Measured on x86_64 (2026-09-22): the path does not merely go
   unexercised, it cannot work as written.* The three-entry `iretq` frame
   `restore` builds for a kernel resume (`RFLAGS`, `0x0008`, `RIP`) was hit
   directly by a `#DB` instrument (item 12): the `iretq` #GPs with the error
   code equal to the CS selector, and the `#GP` entry then re-enters itself
   ~1800 times with the stack descending 0x20 per iteration. Nothing in the
   kernel `iretq`s into a kernel context otherwise — the timer is unmasked
   only inside `restore`'s user window — so `restore`'s `4:` branch is dead
   in practice today and its breakage is invisible; it will have to be fixed
   (or the resume rebuilt the way `#PF` does it) before any of the coverage
   above can be added.
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
   - VM `do_vfs_reply` completes the port's VM→VFS requests: they are asynchronous
     (`vm/vfs_request.rs`, `asynsend3` with `AMF_NOREPLY`), so the reply is the only
     thing that can finish them, and a reply whose request id matches nothing is
     reported rather than read as an answer.
   - `clear_pagefault` forwards VMCTL_CLEAR_PAGEFAULT to the kernel.
   - **No allocation inside VFS's VM-request handlers (FDIO, FDLOOKUP,
     FDCLOSE of `do_vm_call`).** The hazard was that those run while VM is
     blocked in `vfs_request_sync`, and a server's heap growth is itself a
     *synchronous* VM call (`minix-rt`'s `mmap_chunk` → `vmem::mmap` →
     blocking sendrec), so an allocation there was delivered into VM's
     SENDREC reply slot and wedged both servers — VM already returned from
     the fault handler, VFS waited for the mmap reply, run queues empty.
     VM does not wait on VFS any more (`vm/vfs_request.rs`, asynchronous with
     `AMF_NOREPLY`, so a waiting server cannot absorb the request either), so
     the handler may allocate and the class is gone rather than policed —
     PORTING_PLAN.md finding 58.
     Still true, and unrelated to that: `vm_remap`, `vm_getphys` and
     `vm_unmap` in `servers/src/ipc.rs` are *callers* of VM, so a server
     blocking on VM in those paths is the ordinary shape, not a cycle.
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
11. **`EDONTREPLY` and its neighbours disagreed with C in three places — FIXED
    (2026-09).** `sys/sys/errno.h` has `ENOTREADY -201`, `EDEADSRCDST -202`,
    `EDONTREPLY -203`, `ELOCKED -208`, but `arch-common::ipc` had
    `EDONTREPLY -201` (C's `ENOTREADY`), `ELOCKED -202` (C's `EDEADSRCDST`) and an
    `ELOCKWILLBLOCK -203` that C does not define; `minix-std` and
    `servers/rs.rs` repeated the `-201`, and `kernel/src/ipc.rs` returned a
    third, Linux-valued trio (`-73`/`-132`/`-199`). It was latent because
    nothing returned the pseudo-code until the init-complete reply
    (`do_init_ready`) became its first user, and because nothing compared the
    kernel's values — but ported C compares them *by value*
    (`sef_cb_lu_prepare` returning `ENOTREADY` and its caller checking it is the
    standard live-update shape). All four definitions now carry C's values, with
    `test_minix_ipc_error_codes_match_c` pinning them. (`PORTING_PLAN.md`
    finding 20)

12. **A process VM decides to kill was not killed (2026-09-21, FIXED — the multicall wedge).**
    This is what made every multicall tool look broken, and it was two bugs in the same path, each
    of which alone was enough to stop the process from ever dying.

    VM's `sys_kill` called the kernel crate's `cause_sig` **directly**. VM links its own copy of
    the kernel crate, so that wrote `SIGNALED|SIG_PENDING` into VM's private BSS, where nothing
    reads it (`vm_init_boot`'s own comment says the kernel's `Proc` table is unreachable this way).
    A process VM decided to end — the "no region covers this address" branch of
    `handle_pagefault_for` — was instead resumed on the page it had faulted on and re-faulted
    immediately, for good. Measured: 370,708 faults at `0xc00008013` in 25 s, every server idle in
    `RECEIVE`, no process ever dying, and the shell never reaching a prompt (the foreground form of
    every report of this). It now calls SYS_KILL (kernel call 6), the way `clear_pagefault` next to
    it calls VMCTL.

    PM then dropped the signal anyway: `process_ksig`'s catch-all arm delivered only signals that
    PM had *itself* pended (`mp_ksigpending.sigismember`), and a kernel signal for a running
    process is never in that set. C's `process_ksig` hands every signal to `sig_proc` with
    `ksig = TRUE` (`pm/signal.c`, `check_sig(id, signo, TRUE)`) and refuses only "gone" processes.
    Measured with the first bug fixed and this one not: `K: cause_sig p=19 11` fired **174,215**
    times and the child still ran. Fixed to match C (deliver, and skip `IN_USE | EXITING` states).

    With both fixed, the same reproducer gives: one SIGSEGV, the child dies, the shell returns to
    its prompt. `just test-coreutils-wedge` moves from "step 1 never comes back to a prompt" to
    step 2, `0 wedge-big.txt` where `3 wedge-big.txt` is expected — the child now fails visibly
    instead of wedging the machine, which is what makes the next bug findable.

    What the child dies of is a second, unrelated defect and is what remains open: it faults
    dereferencing `0xc00008013` in `clap_builder::parser::matches::matched_arg::MatchedArg::infer_type_id`
    (`rip=0x10b5beb`, `cmpq $0x1,(%rsi)` with `%rsi` the bad value), with an otherwise sane stack
    (`rsp=0xfeff460`) — its own data is corrupted. The same value appears in the earlier
    `sort`/`base64` reports and in item 12's original clap `TypeId` panic, and it is not present
    anywhere in the binary, so it comes from runtime memory (a message-shaped word: its low half
    reads as the child's own endpoint in one run). The VM→VFS request path is *not* the cause:
    with a full trace of both sides, 989/989 requests were delivered and answered; the only
    "unanswered" ones were deferred while VFS was blocked elsewhere and were served later.

    Instruments that settled it, for the next round: `VM: req`/`VFS: work|vmc|rpy`/`VM: rcvd`
    traces on both sides of the async protocol, `K:sd defer|armed` and `K:one goto|stay|
    dswitch|nrskeep` in `try_deliver_senda`/`try_one`, `K: set sendrec dst mtype` and the
    `REPLY_PEND` transitions in `mini_receive`/`mini_send`/`receive_done`, and the kernel's
    `K: fault p=… rip=… rsp=… addr=… err=…` in `handle_page_fault` (a process's own saved frame,
    which `hangdump`'s `rip` cannot give once the process is dying). Gate: `just
    test-coreutils-wedge` (scenario `tools/smoke/coreutils-wedge.tsv`), still red — now on step 2,
    which it names. Deliberately not in `test-arches`, so a known-open issue is not reported as a
    new one.

    **One half of the corruption is identified and fixed (2026-09-22); one is still open.**

    *Fixed — a file page is mapped into the target before VFS has its bytes.* `start_file_page`
    allocates a frame, zeroes it, and maps it into the faulting process **`MAP_USER | MAP_WRITE`**
    so the FDIO safecopy has somewhere to land, then asks VFS for the file's block. The mapping is
    present, user and executable for the whole time the read is in flight. A page fault blocks one
    *thread*, not the process, and every allocation-heavy tool is multi-threaded (`sort` spawns
    reader/merger threads and a `rayon` pool), so another thread of the same process reaches the
    page, takes no fault (it is present), and runs the zeros. Measured on the `sort` reproducer:
    the `G0000` #GP frame is `rip=0x10c605e` — inside `SipHash`'s `c13_rounds` — and a kernel-side
    walk of the faulting RIP at that instant resolved `0x10c6000` to a frame whose first two words
    were zero, with `V: fill va=10c6000 … w0=10c2c148f6314c0d` (the file's real bytes) arriving
    only *after* the dump. Executing `00 00` (`add %al,(%rax)`) with a non-canonical register is a
    #GP, and the same window read as data is a zero word where a pointer belongs — the `clap`
    downcast panic and its relatives. Fixed by mapping that destination **without `MAP_USER`** (the
    FDIO copy is a kernel-mode safecopy through the target's CR3, which does not need the user bit)
    and by having a second thread's fault on a page whose fill is in flight return `Pending`
    (`vfs_request::page_pending`) instead of `Done`, so it stays blocked until the fill lands and
    the completion clears the group. Verified: `just image-x86` green (3 steps, no `K:`/`V:` lines),
    clippy clean, host tests green, riscv64/aarch64 build.

    *FIXED 2026-09-22: the kernel clobbered the process's vector registers (last sub-entry). —
    `seq` dies on a pointer whose bytes are a VMCTL message.* `coreutils seq` — with
    or without arguments, but **not** `--help` — faults reading `0xc00010013`
    (`err=0x4`, no region, so `handle_pagefault_for` SIGSEGVs it) in the same
    `MatchedArg::infer_type_id`, at the same `rip=0x10b5beb` (`cmpq $0x1,(%rsi)`, `%rsi` the bad
    value). The value is `{0x0000000c, <the process's own endpoint>}`, i.e. the *last two fields*
    of the `SYS_VMCTL` message VM sends about that process (`{0x62b = KERNEL_CALL+43, 8 = VM, ep,
    0xc}`), so `clap`'s own structure holds a fragment of a kernel-call message. `--help` working
    while every `ArgMatches` value access fails localises it to the parse result, not to the
    `Command` tree. Not the free path: with a probe on `free_address_space`'s level-0 leaves, the
    child's exec-time teardown frees exactly **one** page with no `PhysBlock`
    (`va=0x3ff00000 fr=0x6035000`); all 527 of its inherited leaves are registered by
    `cow_setup_fork` (`leaf=0x20f reg=0x20f wr=0`), so frames are not being freed out from under
    the parent wholesale. Reproducer and instruments used: `coreutils seq 3` (or `seq 1 3`, or bare
    `seq`) via `FEED_SCENARIO=target/tmp/seq.tsv sh tools/smoke/feed.sh …`, the `K: pf
    addr=… err=… rip=… nr=…` line in `handle_page_fault` (the faulting user RIP is in `p_reg`;
    `hal::read_frame_ip`), and the `V: pf`/`V: fill` region traces in `handle_pagefault_for` /
    `finish_page`. A QEMU-monitor/gdbstub recipe also works and needs no kernel rebuild: break on
    `exception_gpf_entry` (`0x218df0`) or on the user RIP, then read the trap frame off the kernel
    stack — `target/tmp/gpf-probe.py`.

    *Where the message fragment is sitting (2026-09-22).* Stopping on the faulting instruction
    (`target/tmp/seq-probe.py`, a hardware breakpoint on `0x10b5beb`) and reading `%rsi` per hit
    gives the call to `infer_type_id` whose argument is unaligned and unmapped — `%r13`/`%rsi` =
    `0xc00010013`, the same value the kernel fault line names. Searching the child's own memory for
    those 8 bytes finds the **complete** 16-byte record at three places: its stack twice
    (`0xfeffd30` and the `{endpt, cmd}` half at `0xfeff4e8`) and a **freshly zeroed** mmap page
    (`0x10000fba8`, 0x30 bytes of zeros before it). The record is
    `{0x62b = KERNEL_CALL+43 (SYS_VMCTL), 8 = m_source (VM), 0x10013 = endpt (the child), 0xc = 12}`
    — a `VMCTL_CLEAR_PAGEFAULT` kernel call **by VM about this child**, i.e. bytes out of VM's own
    call message. `m_source = 8` is what makes it VM's buffer rather than the child's own: a call the
    child made would name the child there. So a `SYS_VMCTL` message VM built is present in a
    process's stack and in a page that must have been zero when the child mapped it. The layout is
    `sys_vmctl_clear_pagefault`'s buffer byte for byte (`minix-rt/src/lib.rs`: `msg[8..12] = who`,
    `msg[12..16] = 12`), with the kernel's own call header `{KERNEL_CALL+43, src}` in front of it —
    written by `sys_kernel_call_handler` into `kbuf` and echoed back to the caller by
    `kernel_call_finish`, which is why the header travels with the reply.

    *The three instruments that came up empty (2026-09-22).* Each was built to test one structural
    way VM's buffer could reach another process, and each was validated before its silence was
    believed:

    * **Frame reuse at allocation** (`K: REUSE-alloc` after the `VM_PAGING_ALLOC` zeroing, plus
      `REUSE-execstack`/`REUSE-execbrk` after the two `alloc_phys_contig` sites in
      `exec_elf_for_target`): no frame handed to a process is still mapped in VM's own stack or
      heap. Validated by a one-shot baseline that walks VM's ranges and resolves 256 VAs in each
      (`range=1 cr3=0xff26000 n=0x100 first=0x299b000` for the stack), so a "no reuse" answer means
      something.
    * **Stack aliasing** (`K: ALIAS-stack`, run at each exec): no live process's stack shares a frame
      with VM's stack. This is the one an alloc-time check *cannot* see, because a forked process
      inherits its parent's stack frames as COW mappings rather than allocations.
    * **A misdirected kcall reply** (`K: kcf-mismatch` in `kernel_call_finish`, which writes to
      `p_delivermsg_vir` with a raw store and no root check): it never ran with a root other than
      the caller's. Note `kernel_call_resume` is the path that would break that "always the caller's
      root" property, since it runs in the resumer's context for a suspended caller.

    Two more holes of the same shape were then added and are *also* empty (2026-09-22):
    `delivermsg`'s no-switch branch (`K: dm-noswitch`, taken only when the target's `p_cr3` is 0) and
    `virtual_copy`'s boot-root fallback for a *user* process (`K: vcopy-dst-boot`/`vcopy-src-boot`) —
    all three fire zero times across the reproducer. Worth keeping in mind for any future work here:
    `copy_from_user` already does the write side correctly by walking the *target's* tables and going
    through the identity map, and there is no `copy_to_user` at all — so `read_from_proc`'s switch
    (item 15) could equally be expressed that way.

    That leaves the writer unidentified after five candidates ruled out by measurement (frame reuse,
    stack aliasing, `kernel_call_finish`'s root, and the two above), and the next step is not another
    hypothesis but a watchpoint: the record's second sighting is in a *freshly zeroed* mmap page, so a
    write watchpoint on the bytes it occupies should catch the store itself. `target/tmp/seq-probe.py`
    is already the right harness (hardware breakpoints over the gdbstub, no kernel rebuild); it needs
    the mmap VA from a run in the same build and a `Z2,<addr>,16` instead of the RIP breakpoint.

    *The watchpoint was run, and the pages turned out to be private (2026-09-22).* `target/tmp/wp3-seq.py`
    armed `Z2` write watchpoints on both halves of the record's mmap VA and of the stack sites and
    typed the reproducer. No store to the mmap window ever fired, and the stack windows collected only
    ordinary per-process stack churn — but the run ended while boot-time `coreutils` tools were still
    starting, so `seq` had not run yet and the silence is inconclusive rather than negative.

    What *is* decisive is a page-table comparison at the crash (`target/tmp/ptw5-seq.py`: read VM's
    root out of the **kernel's** `PROC_TABLE_ALIGNED` — `0x237680`, `size_of::<Proc>() = 0x370` —
    since VA == PA for RAM in every root, so both roots can be walked at one stop). The child's stack
    page `0x0feff000` resolves to a **private** frame `0x6132000` and VM's own stack page at that same
    VA to a **different** private frame `0x2a99000`; the child's mmap page `0x10000f000` resolves to
    frame `0x65cc000` while VM maps that VA as a supervisor huge physmap page. So the bytes were not
    aliased into the child — something *wrote* VM's message buffer into pages the child owns, and the
    `rsp+0x88` slot clap loaded `%rsi` from (`0x0feff4e8`) is one of those writes.

    The size is the sharpest clue: the mmap page holds the record followed by **40 zeros**, and the
    child's own `{ptr,len}` pair survives at offset `0x38` — exactly the shape and exactly the length
    (56 = `size_of::<Message>()`) of `kernel_call_finish`'s reply copy of a `[u8;64]` `SYS_VMCTL`
    buffer, landing in a page the child had otherwise not written. Adding the `delivermsg`-shaped root
    guard to `kernel_call_finish` (it now switches to the caller's CR3 for the copy and restores,
    `crates/kernel/src/system.rs`) leaves `just test-coreutils-wedge` red on step 2, so the reply's
    root is still not the mismatch — consistent with the `K: kcf-mismatch` result above. Two
    neighbouring candidates were also dismissed by inspection: `vm_memset`'s non-seam path (item 15)
    still ignores `proc`, but nothing in the tree calls `SYS_MEMSET`/`SYS_SAFEMEMSET`, so it is
    unreachable today; and both `exec_elf_for_target`'s stack/brk setup and VM's demand-paging path
    now zero the frames they hand out (the former with a comment naming this very symptom). So the
    next instrument is on the *destination* side of the reply write: log when `vir`'s frame, resolved
    in the *caller's* tables, differs from the frame the loaded root maps for `vir`, and when a
    `Message`-sized store lands anywhere the caller does not own.

    *The mmap sighting is the child's own rehash, and the stack frame was never remapped (2026-09-22).*
    Watching the **quiet** sites — the mmap record slot and the identity aliases of the frames — rather
    than the stack VAs (every process's startup touches those, which cost 83 stops of boot churn and
    kept `seq` from ever running) caught the writer of the mmap copy outright: `Z2` on `0x10000fba8`
    fired with `rip=0x122154d`, inside `hashbrown::raw::RawTable::reserve_rehash`, **with the child's own
    CR3**. So that page's record is the child *relocating an already-corrupt element* into a fresh
    table — a consequence, not the cause. The cause's site is the stack slot `rsp+0x88` (`0x0feff4e8`),
    which is where clap loaded `%rsi` from. Two `memset` watchpoint hits on the identity aliases
    (`0x61324e8`, `0x613fd20`, `cr3=0x101000` = boot CR3) are `exec_elf_for_target`'s stack zeroing, and
    a crash-time walk resolves `0x0feff4e8` to frame `0x6132000` — the same frame that zeroing covered —
    so the page was not remapped: the bytes were **stored into the child's own zeroed frame** and the
    child then propagates them (onto its stack, into a rehashed table). Practical notes for the next
    attempt: watch the identity aliases or the mmap slot (quiet), keep a breakpoint on `0x10b5beb` as a
    wake-up because the corruption can appear with no watched store, and do not query the monitor on
    every stop. One unresolved oddity: scanning `PROC_TABLE_ALIGNED + 0x100` over slots 0..29 finds
    VM's own CR3 at index 13 but **no** entry holding the child's `0xfc26000`, so the kernel's
    authoritative CR3 for a user process is not at that offset of a `5 + slot` entry the way a boot
    slot's is.

    *No hand-out leaves a page unzeroed, so the record is a written value, not a stale frame
    (2026-09-22).* The instrument proposed for this — trap any page that reaches a process non-zero —
    turned out to have nothing to catch, and that is a real result: `VM_PAGING_ALLOC`
    (`do_vm_paging_handler`) zeroes every allocated page through the identity map, with a comment
    naming this very symptom; `exec_elf_for_target` zeroes the stack and the pre-mapped brk window
    explicitly; the demand-paging path zero-fills its page through a scratch mapping before mapping
    it in; and `do_brk`'s heap-growth path draws from `vm_alloc_pages`, i.e. from that zeroing ALLOC.
    So every path that hands a page to a process clears it, and the record must instead be **stored
    into the child's already-owned frame**. Together with the rehash catch, that says the corrupt value
    travels inside the child's own data from a point before clap reads it, and the origin to look for
    next is a byte copy into the child through its own address space (a `vircopy`-shaped write from
    VM's buffer) or the child's exec-time `argv`/frame content — not a frame the allocator recycled.

    *The `vircopy` candidate was then trapped and came up empty (2026-09-22).* A temporary probe in
    `virtual_copy`'s bounce loop scanned each chunk's *source* bytes for the full `SYS_VMCTL` call
    header (`2b 06 00 00 08 00 00 00`) and printed `K: vcopy-kcall src=… dst=… len=…` when it appeared:
    **zero hits** across the wedge scenario, so no cross-address-space kernel copy carries VM's message
    buffer into a process. The probe was verifiably live rather than dead — with the filter loosened to
    the bare 4-byte call magic it fired six times, all page-sized (`len=0x1000`) fills of the read path
    (`src=<stack VA> → VFS`, then `VFS → slot 19`), none carrying VM's endpoint as the word after the
    magic. That leaves the two candidates above narrowed to one: a copy that does not go through
    `virtual_copy` at all (`delivermsg`'s 64-byte delivery, or the 56-byte `kernel_call_finish` reply,
    whose length matches the `Message`-sized window seen in the child's page), or the stale frame
    content of a page already handed to the child from one of those two paths.

    *Both remaining message writes were then trapped and are empty too (2026-09-22).* Temporary probes
    scanned the staged message in `delivermsg` (`p_delivermsg[..56]`) and the reply in
    `kernel_call_finish` (`msg[..56]`) for the same `2b 06 00 00 08 00 00 00` header. `delivermsg`
    never delivers one (0 hits): no message handed to any process carries a kcall buffer. And
    `kernel_call_finish` never writes one to a caller other than endpoint 8 nor with a CR3 that differs
    from the caller's (0 hits under exactly that filter) — so every `SYS_VMCTL` reply in the run went
    back to VM itself, in VM's own root, which also re-confirms `K: kcf-mismatch` from the other side.
    With that, **every** writer of a `Message`-shaped buffer into a user address has now been trapped
    and come up empty, and so has every path that could hand a process a stale frame.

    *The endpoint reading is confirmed, from the kernel's own dump (2026-09-22).* The caveat that the
    `src=8` / `who=0x10013` attribution was inherited from an earlier session is now closed: the shell
    builtin `hangdump` (`SYS_HANG_DUMP`, one `ep=`/`rip=` line per live slot) shows this build's map —
    boot processes are slots `0x00..0x12` with the endpoint *equal to* the slot (`vm ep=00000008` among
    them), and a forked child is `init*F ep=00008013`: slot 19, generation `0x80`. The crashed child's
    `who=0x10013` is that same slot 19 with generation `0x100`, and `src=8` is VM. So the record really
    is **VM's** `sys_vmctl_clear_pagefault` buffer naming the child, and it really is sitting in the
    child's own zeroed, private pages. (Reproducer: `target/tmp/ep.tsv` — `coreutils seq 5 &` then
    `hangdump`, so a coreutils child exists in the table when the dump runs; the same dump also shows
    that backgrounded child PAGEFAULT-blocked at its ELF entry, `rip=0x1000000 rts=0x400`,
    KNOWN_ISSUES 14.)

    *What is left, given every trap is empty (2026-09-22).* With the endpoint reading confirmed, the
    facts stand as: VM's `sys_vmctl_clear_pagefault` buffer appears in the child's own private,
    zeroed-at-exec stack frame (at `rsp+0x88`, the slot clap loads `%rsi` from) and in a fresh mmap
    page — and the mmap one is the child's own `hashbrown` rehash relocating an already-corrupt
    element, so the child had the bytes in its data first. No kernel copy of a `Message` into a
    process's address (`virtual_copy`, `delivermsg`, `kernel_call_finish`) and no stale-frame hand-out
    exists, and nothing wrote the child's frames through their identity aliases except the exec-time
    zeroing. That points at the one writer family not yet trapped: the **grant/safecopy** path
    (`SYS_SAFECOPYTO`/`SYS_SAFEMEMSET`-shaped, i.e. `write_to_proc` and `grants.rs`), whose *source* is
    a server's buffer and whose destination is a user buffer named by a grant — exactly the shape that
    would put a server's stale message bytes at the front of a process's fresh buffer, and the one
    place a 56-byte `Message`-shaped window can still arrive from outside the child. A second, broader
    thing to audit while there: `exec_create_root` copies "the identity map with **user access** in the
    low window", so a new process can reach low physical memory (including other processes' frames) at
    VA == PA — a channel no kernel-mediated copy needs to be involved in, and worth measuring on its
    own.

    *The grant/safecopy family came up empty too, and the harness was racing the shell (2026-09-22).*
    `grants.rs::safecopy` has two bare on-x86 copies that `virtual_copy`'s trap could not see: the
    `CPF_TRY` path's direct `copy_nonoverlapping(src_addr, dst_addr, bytes)` (no root handling at all)
    and the direct copy it falls back to when `virtual_copy` returns non-zero. Both were trapped — the
    TRY path by reading its source through `read_from_proc` (which switches roots) and scanning it for
    the same `2b 06 00 00 08 00 00 00` header, the fallback by printing unconditionally, since on x86
    reaching it at all would be news. Result: **0 hits**, and the fallback never ran, so that family is
    closed as well.

    The more useful outcome is a harness bug this exposed. `tools/smoke/feed.sh` says it in its own
    comment — "input written to the console before the shell is reading it is *dropped*, not buffered"
    — and every probe here that typed straight into QEMU's stdin after matching the prompt was racing
    it. That is why those runs were full of boot churn and never reached the crash: their commands were
    being dropped, so the guest sat at the prompt while the watchpoints stayed armed. A watcher that
    arms on *proof* the shell is live (`/bin/echo ARMx` and wait for the echo, retrying, or better still
    let `feed.sh` drive the input and arm on its own echo line) is already written and ready to run:
    `target/tmp/wp11-watch.py` with `target/tmp/wp11.tsv`
    (`FEED_SCENARIO=target/tmp/wp11.tsv sh tools/smoke/feed.sh …` with `-gdb`/`-monitor` added to the
    QEMU command, the watcher armed from the same log). That is the run that should finally catch the
    store into the child's own stack slot, which is the one write no trap has covered.

    *The coordinated watcher works, and saw no store (2026-09-22).* With `feed.sh` driving the input and
    the watcher keyed off its `ARM` answer (`target/tmp/wp11.tsv`, `target/tmp/wp11-watch.py`), the
    three things that had been defeating this attempt are fixed and measured: a gdbstub client that
    connects **leaves the vCPU stopped**, so the watcher must `c` before waiting (otherwise the guest
    never boots and the feeder never gets a shell — the log showed only SeaBIOS output); the serial
    console emits `\r`, so a `^ARM$` match has to allow it; and arming right after the shell *answers* a
    command is what avoids the boot churn. In that run the watchpoints armed cleanly
    (`Z2 0xfeff4e8 -> OK`, `Z2 0xfeffd20 -> OK`) just before the feeder sent `coreutils seq 3`, and then
    **no stop packet arrived at either slot** while the child ran and died — the socket was reset when
    the feeder finished. So the bytes clap reads at `rsp+0x88` were not stored there at run time by
    anyone the watch could see. That leaves the frame's *origin*: if the memset verified earlier
    (frame `0x6132000`, `cr3=0x101000`) was the **shell's** stack and not the child's, then the child's
    stack page is a different frame that was never zeroed and inherited its content across the
    fork/exec transition. The instrument for that is one line: print the *endpoint of the process whose
    stack is being zeroed* alongside the frame address in `exec_elf_for_target`, plus the same for the
    brk window, so the frame the child ends up using can be matched against the frame that was
    cleared.

    *That instrument answers the ownership question (2026-09-22).* Run with the coordinated feeder, it
    printed one line per exec naming the process whose frames are being zeroed: `K: execmap stack
    ep=0000000a frame=000003e25000 pages=0x100` and `brk ep=0000000a frame=000005f29000` for `init`,
    then `stack ep=00008013 frame=000006034000` and `brk ep=00008013 frame=000006134000` for the
    **forked child**, in the same run. So the child's own stack and brk really are the frames being
    cleared: the memset verified by watchpoint earlier belongs to the process whose stack it is, not to
    an unrelated one, and "the child inherited an unzeroed stack" is closed. The store that puts the
    record into that cleared stack is still the unexplained step, and this run could not observe it
    because the watcher's `\x03` interrupt (sent to arm the watchpoints) appears to leave the gdbstub
    unable to take a subsequent RIP breakpoint — the `Z1` was accepted, the guest ran, and no stop ever
    arrived. The next attempt should arm while the CPU is still in its initial stopped state (accepting
    boot churn and filtering stops by reading the site) rather than interrupting a running guest.

    *The stack sighting is a register spill, not a write from anywhere (2026-09-22).* Armed from
    connect (no interrupt, `feed.sh` driving the input), the watchpoint on `0x0feff4e8` fired at
    `rip=0x10b5be4` with `rsp=0xfeff4e8` itself — and disassembling `infer_type_id` shows that RIP is
    the prologue's `pushq %r12`, so the store that was caught is the preceding `pushq %r13`: the
    "record" at that slot is simply `infer_type_id` saving `%r13 = 0xc00010013`, the bad value already
    in a register on entry. The two stack sightings are therefore *consequences* (saved registers),
    and the faulting `cmpq $0x1,(%rsi)` is taking `%rsi` = the same value passed in as `&MatchedArg`.
    Following it back one frame: `ArgMatches::get_many::<String>` at `0x1209a92` calls
    `flat_map::FlatMap::get`, which returns that pointer in `%rax`; `0x1209a9c`-`0x1209aa7` put it in
    `%r13`/`%rsi` and call `verify_arg_t`. So **clap's `FlatMap` storage holds the message bytes where
    a `MatchedArg` pointer belongs**, which is exactly what the mmap sighting showed the hashbrown
    rehash relocating. The chase is now narrow and well-posed: watch the *FlatMap storage* for the
    write that puts `{0x10013, 0xc}` there (address from `FlatMap::get`'s disassembly plus the crash
    registers, armed before the child's clap runs). And because every kernel copy path is now trapped
    empty, the one channel by which another process's bytes become *readable* by the child without a
    copy is item 16 (the user-accessible low identity window) — which is why it is filed separately
    and should be measured next.

    *The writer is the child's own code, relaying bytes that are already in its frame (2026-09-22).*
    Three instruments settle the shape of the remaining step; run them with the guest driven directly
    from Python (`target/tmp/wp-drive.py`), because `feed.sh`'s pipeline intermittently dies on this
    host (`couldn't create signal pipe, Win32 error 5`) and takes QEMU down with it, which resets the
    gdbstub mid-run.

    * The same VA in two processes' stacks is not the same memory. At the crash, the child's page for
      `0x0feff000` is one frame and VM's is another, and scanning **every** live process's root for
      **every** 4 KiB of `0x0fe00000..0x0ff00000` finds no root in which that frame is mapped at all.
      So the record did not arrive by another process writing "the same address": the child's stack
      page is private, and nothing else can reach it.
    * The child makes no kernel call. A probe gated on `call_nr == 43` (SYS_VMCTL) and `p_nr >= 18`
      printed nothing across three runs, and a probe of the whole kcall path gated on
      `p_endpoint >= 0x1000` printed nothing either — so the `{KERNEL_CALL+43, 8, ep, 12}` header was
      not produced by the child. `kernel_call_finish` is also cleared for good: with a probe printing
      whenever the loaded root differed from the caller's, no mismatch ever occurred, so every reply
      copy lands in the caller's own space.
    * The child has no second thread (`p_t_next` is self), so no thread-stack collision.
    * A write watchpoint on the slot the fault reads (`0x0feffd28`) catches **the child's own code**:
      `uu_seq::uumain` (and the same instruction in `uu_unexpand::uumain`, which does not crash) does
      a 56-byte struct move, and the slot it writes is that struct's `+0x18` field. Disassembling it
      shows the source of the value is a *stack slot in the same frame* (`0xfeff870`), i.e. the record
      is already in the child's frame before this copy and the copy only relays it. The struct is 56
      bytes with a `Vec`-shaped triple at `+0` and another at `+0x18` — `ArgMatches` is
      `FlatMap`(48) + `Option<Box<_>>`(8) = 56, and its `values` `Vec`'s ptr/cap are exactly the two
      quadwords the record occupies.

    What remains is a single write into the child's private frame, somewhere between its exec (where
    the frame is measured zero, including the two slots above) and the first clap access, whose writer
    the slot watchpoint did not catch — the suspects are a store QEMU's virtual watchpoint does not
    report as a store at that address, or a kernel copy that reads the target's address space without
    the kernel having this process's root loaded. Instrument to try next: watch the *source* slot in
    the child's frame (`0xfeff870`) rather than the clap field, read the slot's bytes **and the live
    CR3** on every hit (a hit can otherwise be a different process's frame at the same VA, read under
    the wrong root), and report the hit only when the bytes at that moment are the record.

    *Every channel that could write it has now been measured empty (2026-09-22).* The write lands
    after exec and no candidate path carries it:

    * **Zero at exec.** Reading the two slots back through the new root inside `exec_elf_for_target`,
      immediately after the frame is built, gives `d870=0 dd28=0` — so the bytes arrive *after* exec,
      and "the exec frame build wrote them" is closed.
    * **No alias.** At the fault, the child's root has its stack frame mapped at **no other VA** in
      `0x01000000..0x02000000`, `0x0fe00000..0x0ff00000` or the first 16 MiB above the mmap base, and
      no other live process's root maps that frame anywhere in the stack window (both scans print
      nothing). So no address in any space reaches the child's frame except the child's own stack VA.
    * **No physical-destination copy.** `virtual_copy` with `dst_proc < 0` — which writes `dst_addr`
      as a *physical* address, since it resolves to the identity map — does happen and is hot (VM and
      rs, 0x50 bytes at a time, to `0x38a730`), but never near the frame. `write_to_proc` is never
      called with an address below 256 MiB.
    * **No delivery.** `delivermsg` for the child targets `0x0fefbc00`, `0x0feffbe0`, `0x0feffe40`,
      `0x0feffe60`, `0x0feffe90` and carries clean `{m_source=8, m_type=0, ...}` replies; reading the
      two slots back straight after every one of those deliveries finds no record there.

    So the record's arrival is invisible to (a) a virtual watchpoint on either slot, (b) a scan of
    every root for the frame, (c) the physical-destination and low-address copy paths, and (d) the
    exec frame build. Arming the watchpoints *late* is the way to test the remaining suspicion — a
    store through the identity map, whose virtual address (`frame + 0x870`) is a *different* VA from
    the stack slot — but two attempts failed for a harness reason worth recording: watching a stack VA
    from boot stops every process on every stack access and the guest then never reaches clap inside
    the run; and a `Z1` at the user entry VA `0x1000000` aliases the kernel's identity mapping of the
    same address (16 MiB is inside RAM) and fired 2 698 364 times before the shell prompt. A late arm
    needs a stop trigger whose VA is **not** also a low physical address — anything above 256 MiB that
    user code executes from, which the child's `0x01000000` text is not.

    *The identity channel is closed too, and with it every write path (2026-09-22).* Arming `Z2` on
    the frame's **own identity address** (`frame + 0x870` for the record's start, `frame + 0xd28` for
    the clap field) costs nothing to run — user code never touches a supervisor-only low VA — so it
    can stay armed for the whole boot. In a run in which the corruption demonstrably happened (the
    child died silently: the console shows the command and then the prompt, with no output), those two
    watchpoints fired **zero** times. This is the last channel: on x86_64 physical memory is reached
    through the identity map and nothing else — `kern_map_physical`, the only other route, has no
    callers, and `KERNBASE` is the kernel's link address rather than a physmap. The child's root also
    maps its frame at no other VA across text (16 MiB), the stack, the **brk heap** (`user_heap_base
    = 0x3fe00000` + 2 MiB, the range earlier scans missed) and the first 16 MiB above the mmap base.

    Two instrument traps this round cost runs, both recorded in the `bare-metal-debug` skill: the
    stack frame the allocator hands the child **shifts by a page between builds** (`0x6132000`,
    `0x6133000`, `0x6134000` in consecutive builds), so identity slots must be read from the same
    build's exec line rather than carried over; and `proc.stdout.read(65536)` on a Windows pipe
    blocks until the buffer is full, which reads as an empty guest log and a guest that "never
    booted" (use `os.read(fd, 65536)`).

    **What is left, stated as the contradiction it is.** The child's own code is caught *moving* the
    record (`0x103200b`/`0x1003ff2`, in `uu_seq` and `uu_unexpand`, CR3 = the child's) out of the
    slot `0xfeff870` of its own frame — yet no write *into* that slot is observable: not by a `Z2` on
    that VA (armed from connect in the run that caught the move), not by a `Z2` on the frame's
    identity address, not by any of the scans above. The value's arrival is therefore the one thing
    left to explain, and the instruments that can see it are the ones not yet tried: a QEMU TCG
    plugin or `-d`-style write trace (watchpoints are per-VA and cannot observe a write whose VA is
    unknown), or trapping the *exec* path's own writes into the frame (the argv/envp string area) by
    reading the frame back at each step of `exec_elf_for_target` rather than only at the end.

    *The "stale or uninitialized field" reading, tested (2026-09-22).* If the child's `ArgMatches`
    field is never initialized, the bytes it reads must be stale from *somewhere* — most plausibly
    its own earlier use of the same stack address (a message buffer in a popped frame). A probe that
    scans the child's **whole 1 MiB stack** for the record's bytes at *every* user fault answers it:
    the pattern is present **nowhere** in the stack at any fault before the crash, and at the crash
    (faulting `0xc00008013`) it is present only at the slot the child reads, `0xfeffd28`. So the value
    is not sitting in the child's stack earlier in its life: if the field is uninitialized, its stale
    bytes come from a **register** or from outside the stack (heap, a global, or a message payload).
    Two things this also settles: the child's `p_endpoint` really is `0x8013` (so "a child's kernel
    call stamped as VM's" is dead — such a call would stamp `0x8013` and the endpoint-gated probes
    would have caught it), and `2b 06 00 00` is **not a safe signature** — it matches any value whose
    low half is `0x62b`, which a legitimate small integer at that slot produces; the full 8 bytes
    (`2b 06 00 00 08 00 00 00`) or the `{ep, 12}` pair must be used.

    **Method for the next attempt: taint the copy one hop per run.** The destination is a
    build-independent *VA* (`0xfeffd28`) and a `Z2` there catches the store; the store's source is a
    stack slot in the child's own frame, whose address can be read off *that build's* disassembly of
    the reported RIP. Arm a watchpoint on that address, repeat, and the chain walks back one copy at
    a time until a write has no source in the child's frame. This is worth doing precisely because my
    earlier source watchpoint never fired: the frame shifts by a page between builds, so a
    *guessed* source address (`0xfeff870`, derived from one build's disassembly) is the likely reason,
    not the absence of a write. Caveat for that sampling: once the child's text is paged in there can
    be no faults at all until the crash, so "read it at the next fault" has very coarse granularity
    late in the run — a write watchpoint is the only instrument there.

    *Hop 1 of the taint walk, attempted (2026-09-22) — and why it did not land.* The destination VA
    (`0xfeffd28`) is build-independent, but it is **hot**: VM's own call buffer lives at that same VA
    and is written on every kernel call, so watching it from connect costs ~6 200 stops and the child
    never reaches its exec inside a 300 s run. Arming it *late* needs a stop, and the natural trigger —
    the child's first heap write — is not process-specific either: `user_heap_base()` (`0x3fe00000`)
    and the mmap base (`0x100000000`) are the **same VA in every process**, so the first hit is the
    shell's heap during boot and the arm happens before the child exists. The fix is in hand (keep the
    cold watchpoint armed, read the live CR3 at each of its few hits, and arm the destination only when
    that CR3 is one the exec probe has reported for a forked endpoint) and was being tested when the
    run stalled: the guest's log ended **mid-echo** with only 10 stops, which is a harness failure —
    QEMU blocked on a console pipe a died-silently reader thread had stopped draining — not a guest
    state. So the taint walk's first store is still uncaptured. Next: make the reader observable (and
    its death fatal to the run), re-run the CR3-gated arm, read the store's source operand off that
    build's disassembly of the reported RIP, and repeat — one hop per run.

    *Re-run with the reader hardened (2026-09-22): the stall is the guest, and the watchpoints cause it.*
    With the reader made honest (thread-safe, its death fatal, progress logged every 20 s) the same
    script no longer blames the harness: it reports 10 stops, a console frozen at 2 946 bytes, and a CPU
    that is *not* stopped — i.e. the guest itself stops making progress, with the shell having echoed
    the command and neither the shell's nor the child's exec line ever printed (`K: exroot` appears
    once, for `init`). In the uninstrumented build the same command execs and dies every time. So the
    two cold-slot watchpoints perturb fork/exec enough to hang it before the child exists — the
    instrument changes the system under test. That is the real obstacle to hop 1, not the kernel, and
    it points away from watchpoints entirely: a TCG plugin (or `-d`-style write trace) observes writes
    *without* stopping the CPU, which is what this step needs.

    *The "live VMCTL copy" and "stale slot" readings are both measured false (2026-09-22, no watchpoints).*
    Both were tested in one run with a single instrument that never stops the CPU, and the result narrows
    the chase from *where the bytes came from* to *where the value came from*. Reproduction is the gate
    as it stands (`just test-coreutils-wedge`, red on step 2: `coreutils seq 3 > wedge-big.txt` writes
    nothing while `coreutils wc` reads the empty file back).

    The instrument: `minix-rt::sys_vmctl_clear_pagefault` stamps `0xA55A_5AA5` in the message word the
    handler never reads (`[16..20] = value`), so a copy of that live buffer is distinguishable from any
    stale bytes; `handle_page_fault` then, on every user fault of a forked process, sweeps the page
    holding clap's field (`0x0feff000`), sweeps the whole 1 MiB stack (plus the brk window on a wild
    fault), and prints `addr`/`err`/`rip`/`ep` with a hit count, the first hit, the word after it, and
    the hit count at clap's slot; on a wild fault it also resolves `0xfefd0c0` and `0xfeffd28` in
    **every** live process's tables. The marker's liveness is checked from the kernel side (a probe in
    `do_vmctl_handler`'s `VMCTL_CLEAR_PAGEFAULT` arm printing `who` and `value`), because a silent probe
    proves nothing: it printed `K: vmctl who=0000000a val=a55a5aa5`, so every live
    `sys_vmctl_clear_pagefault` buffer in the run carries the marker.

    What that gives:

    * **The slot is zero until the fault that reads it.** The crashed child (slot 19, `ep=00008013`)
      faults 87 times before it dies; the 16-byte record is present at `0x0feffd28` for the **first**
      time on `pf#086` — `addr=0x0c00008013 err=0x04 rip=0x10b5beb`, the documented `clap` fault — and
      the page sweep found it on no earlier fault. So the bytes are not stale content of that address,
      and no hand-out is implicated (the exec-time sweep of the whole stack + brk is also 0).
    * **The record is not a copy of VM's live buffer.** It is followed by `02 00 00 00`, not the marker.
      The earlier attribution rested only on the `{0x62b, 8, ep, 12}` shape, and that shape is not
      evidence: `0x62b` is `KERNEL_CALL+43` only if read as a header at offset 0.
    * **Nothing else maps those bytes.** At the crash the record exists at `0x0feffd28` in the crashing
      process only (frame `0x6135d28`); every other live process, boot servers included, has zeros or
      its own data at `0xfefd0c0` and at `0xfeffd28`. So no aliasing and no inheritance *at the crash*.
    * **But the child *does* inherit a run of them from its parent, pre-exec.** At `pf#002` — before the
      child's first instruction fetch at its entry (`pf#003`) — its stack holds **14 consecutive
      copies** at `0xfefd0c0`, in the same region as the parent's argv strings
      (`/bin/coreutils\0seq\0`), i.e. in frames it got from the fork and had not yet replaced. Those
      faults are `err=0x07` (present, write, user — fork COW), not the exec build.
    * **The "record" is two adjacent words of a 16-byte-entry table, not an object.** Dumping
      `0xfeffcf0..0xfeffdf0` at the crash shows entries like `{3, 0x0feffe88}`, `{4, 0x1000139f0}`,
      `{0x0feffd70, 0x18}`, `{2, 0x100011bd0}` — ordinary small integers and stack/mmap pointers — and
      the two corrupt words sit as the *value of one entry and the key of the next*, at
      `0xfeffd28`+0, exactly where `ArgMatches`' `+0x18` field (its second `Vec`'s `ptr`/`cap`) falls,
      which is the field clap dereferences. So the object to explain is a `Vec` header holding
      `{0x62b, 8}`/`{0x8013, 0xc}`, not a message-shaped buffer.

    Together those say the corrupt value is **stored by the child's own code in the instruction window
    immediately before it dereferences it** (the store and the load are in the same fault-free window),
    and that the *value* — not the destination — is what has to be traced. The value is also present, as
    a repeated artifact, in the parent's stack region *before* this child runs at all.

    *Instrument that should land next: make the store fault and name itself.* Instead of arming a
    watchpoint (which this host cannot do here without stalling fork/exec — see above), have the kernel
    clear the **write** bit of the PTE for the page holding the destination slot. The child's own store
    then takes an `err=0x07` fault and the `K: pf addr=… err=… rip=…` line already in
    `handle_page_fault` names the **store instruction**; disassembling that RIP *in the same build*
    gives its source operand, which is hop 1 of the taint walk. Re-arming it after each fault makes the
    walk a loop inside one run instead of one hop per run, and the perturbing writes (a single extra
    COW fault on one page) are what the fault path already handles. What to watch for: the destination
    VA is build-independent (`0x0feffd28`) but the page also carries live clap data, so an early
    re-arm reports a *read* first and the sweep has to run until a `rip` whose disassembly stores
    to that slot.

    *That instrument suppresses the bug, and the writer is now confined to one fault window
    (2026-09-22).* Clearing the write bit of the page (`0x0feff000`) from just before the crash onward
    **removed the corruption entirely** — the child ran past `pf#086` and wrote nothing, so the store
    that lands the record goes through the child's *user* PTE at VA `0x0feffd28` (a read-only PTE stops
    it, which a store through the identity map would not be), and the resolution of that fault is what
    then drops the write. So the trick is not a free instrument; what it does buy is the shape.

    Two cheaper instruments did land, and four channels that were still open are now measured closed:

    * **The write is in one fault window.** Logging the live value of the slot (and never anything else)
    on *every* user fault keeps the guest alive (printing every fault on its own perturbed it: the
    `addr=0x0c00008013` crash vanished from a run that printed 291 lines, and came back when only
    changes were printed). The slot holds a plausible `Vec` header until the fault immediately before
    the crash and the record at the crash: `prev#085 addr=0x1032000 rip=0x1031fff slot=0x1182d8b` then
    `pf#086 addr=0x0c00008013 rip=0x10b5beb slot=0x000000080000062b`. Both are instruction fetches into
    `uu_seq`'s code, so the store is in the code between those two RIPs.
    * **The page is never remapped.** The page's frame (`0x6134000`) is constant from the child's third
    fault to the crash, and the record's occurrence count in the page goes 0 → 1 — so this is an
    in-place store, not a frame swap and not a stale hand-out.
    * **`kernel_call_finish`'s reply copy is correct**, which retires the wrong-root reading for good:
    over 20 sampled `VMCTL_CLEAR_PAGEFAULT` calls `live == p_cr3` (`0xff26000`) and `vir` resolves to
    the same frame in both roots (`fl == fp`). VM's call buffers for these sit at `0x0feffd40` and
    `0x0feff7b0` — the same VA page as clap's field, which is why a watchpoint there is hot.
    * **The identity window is closed, completely.** Enumerating all 512 low-1 GiB entries in the
    child's root shows exactly four user-accessible ones — its code (`0x01000000`, `0x01400000`), stack
    (`0x0fe00000`) and brk heap (`0x03fe0000`). No identity entry carries U, so a user-mode read of
    `VA == PA` is impossible and the kernel's `kbuf` is unreachable. (Also: `0xe3`/`0x83` in a PTE are
    *not* U — bit 2 is clear in both; sampling four addresses and reading the hex by eye is how this
    looked alarming for a while. Enumerate, and decode bit 2 explicitly.)
    * **Nothing is aliased.** Comparing the child's stack-window frames against VM's on *every* fault
    finds no shared frame, and at the crash neither process's stack maps the other's frame. The bytes
    exist in VM's stack at `0x0feff7b0` (`pa=0x2a9d7b0`) and in the child's at `0x0feffd28`
    (`pa=0x6136d28`) — *different* VAs, *different* frames, so a copy did move them, not a common page.
    * **Not the delivery path, not the copy primitive.** 16 sampled deliveries to forked processes
    (32 bytes each) are clean: `m_source=8, m_type=0`, plausible payloads, no `2b 06 00 00` anywhere.
    `virtual_copy` into a stack window is used only for the exec-frame handoff — `sp=13 sa=0xfefd0a0
    dp=01 da=0xfeff5d0 n=0x0f` copies the child's own `/bin/coreutils` string (15 bytes) out to a
    server, and nothing copies *into* a forked process's stack.

    What is left is therefore narrow and stated exactly: **the record is written into `0x0feffd28`
    between `rip=0x1031fff` and `rip=0x10b5beb`, by something holding the child's root, through the
    child's own PTE, and it is 16 bytes whose value is the head of a `SYS_VMCTL` call buffer.** The
    `movups %xmm0, 0x18(%rsi)` at `0x1032007` (whose source `0x2a0(%rsp)` = `0xfeff870` does *not* hold
    the record at the preceding fault) is not it; the writer is a different store in that window.
    *(Superseded by the last sub-entry: the observation is what led to the answer, but the
    conclusion is not — there was no other store, the value was already in a register.)* Next
    instrument: instrument every kernel write to a user VA *by address* (not by pattern) for the
    window, i.e. log any copy in `copy_from_user`/`write_to_proc`/`safecopy`/`delivermsg` whose
    destination is `0x0feffd28`±0x40, and, since the window is between two known RIPs, disassemble
    `0x1032000..0x10b5beb` for a 16-byte store whose destination is `[rsp+0x88]`-shaped.

    *Trap by destination address: the kernel does not write the child's copy (2026-09-22).* Every path
    that writes into a user virtual address — `delivermsg`, `kernel_call_finish` (all three copy sites),
    `write_to_proc`, `virtual_copy`, both bare copies in `grants::safecopy`, and `mini_receive`'s direct
    write — was instrumented with that address, and it prints only when the write actually *covers*
    `0x0feffd28`. The only such writes in the whole run are VM's own kernel-call replies:

    ```
    K: kfin ep=000000000008 nr=000000000008 live=00000ff26000 pcr3=00000ff26000 vir=00000feffd10
    K: wk dst=00000feffd10 n=0040
    ```

    `ep=8`/`nr=8` is VM, `live == pcr3` is VM's own root, and the 64-byte reply therefore lands in VM's
    own frame at VM's VA `0x0feffd10` — which covers VM's `0x0feffd28`. It never reaches the child, whose
    `0x0feffd28` is a private frame. So with content traps, address traps, aliasing and the identity
    window all empty, **no kernel path puts these bytes in the child**.

    That makes the shared VA the crux, and it is worth stating on its own: VM's call buffers for
    `VMCTL_CLEAR_PAGEFAULT` sit at `0x0feffd10`, `0x0feffd40` and `0x0feff7b0`, and clap's field is at
    `0x0feffd28` — the *same VA page*, 24 bytes from one of VM's buffers. Any VA confusion anywhere (a
    stale `p_delivermsg_vir`, a copy that does not switch, a walk of the wrong root) reproduces this
    symptom *exactly*, because the two processes' stacks are at identical addresses by construction.
    The remaining possibilities are narrowed to two: a *user* store in the child of a value it read from
    its own frame (so the search must move to the child's *loads*, e.g. by single-stepping the window
    `0x1031fff..0x10b5beb` or by trapping reads of `0x0feffd10..0xfeffd50`), or a user-VA write path not
    in the list above (the exec image copy in `do_exec_load_handler` is the one such copy that is not
    instrumented, and it is the next thing to patch or to short-circuit).
    *The store is finally named by a data breakpoint, and the answer it gives contradicts the
    hop-by-hop method (2026-09-22).* A `#DB` data breakpoint (DR0, write, LEN=8) on `0x0feffd28`,
    armed one-shot from `handle_page_fault` at the child's findfetch fault (`err=0x14`,
    `addr=0x1032000`, `p_endpoint >= 0x1000`) and reported by a `#DB` handler that prints RIP, the
    saved user RSP, DR6, both watched slots and the raw frame, gives the store directly:

    ```
    K: db n=3 rip=000000000103200b cs=000000000000001b frsp=000000000feff5d0 dr6=00000000ffff0ff1 cr3=000000000fc26000
    K: db d0=000000000feffd28 v0=000000080000062b v1=0000000c00008013 d1=000000000feff870 w0=0000000000000004 w1=00000100013a50
    ```

    `rip=0x103200b` is the instruction after `movups %xmm0,0x18(%rsi)` at `0x1032007` (the child's
    own `uu_seq::uumain`, `cr3` the child's), `frsp=0xfeff5d0` the user RSP at the store, and the
    destination slot now holds the record — `{0x000000080000062b, 0x0000000c00008013}`, i.e. the
    `KERNEL_CALL+43` header and the `{ep, 12}` pair. So the *store* is settled: it is a 16-byte user
    store by the child's own code, through the child's own PTE, and the destination is the slot clap
    dereferences. **What it refutes is the source**, and with it the whole "hop back through the
    stack one copy at a time" plan: the disassembly of the same build reads
    `movups 0x2a0(%rsp),%xmm0` / `leaq 0x740(%rsp),%rsi` / `movups %xmm0,0x18(%rsi)`, so
    `0x2a0(%rsp)` = `0xfeff870` should hold exactly what lands at `0xfeffd28`. It does not — at that
    instant `0xfeff870` holds `{0x4, 0x00000100013a50}`, a legitimate `{length, mmap-heap pointer}`
    pair. The two cannot both be true, and the resolution is not another watchpoint: the value is
    already in `%xmm0` when the store runs, so the *load* that put it there is not the one the
    linear disassembly names, and **the record does not come from a stack slot at all**. Walking the
    copies back is therefore the wrong instrument, and the earlier note that the
    `movups` "is not it" was right for the wrong reason.

    *`kernel_call_finish` is where the record's bytes are written, and they never reach a child
    (2026-09-22).* A source-signature probe on every kernel-side copy (`write_to_proc`, both
    branches; `virtual_copy`'s bounce write; `delivermsg`; both `kernel_call_finish` copy sites)
    prints `K: wk <tag> dst=… n=… cr3=… s0=… s1=…` when the source begins with the **full eight
    bytes** `2b 06 00 00 08 00 00 00` (the bare 4-byte magic is not a signature — item 12 above
    records why) or when the destination is one of the two slots. Every hit is a
    `kernel_call_finish` reply written with **VM's own root** (`cr3=0x0ff26000`) into **VM's own
    stack** (`dst=0x0feff7b0`, `0x0feffef0`): `s0=0x000000080000062b`. The record is VM's
    `SYS_VMCTL` kernel call *reply*, echoed back to VM with the kernel's own call header in front of
    it — exactly the layout `sys_kernel_call_handler`/`kernel_call_finish` produce. With VM's root
    excluded, the same probe reports **zero** hits in the whole wedge scenario: no kernel-side copy
    under any other process's root carries those bytes into any process's stack window. So the
    record reaches the child through the child's **own** buffer, and every kernel-mediated route is
    now not merely suspected but measured empty.

    *A trap taken at CPL0 cannot be returned from, which constrains every future instrument here
    (2026-09-22).* Arming the watchpoints early (at exec, so the whole post-exec window is covered)
    wedges the guest: the first `#DB` that arrives while the CPU is in the kernel (one was caught in
    kernel `memcpy` at `0x21e026`, writing `0xfeff870` under a process root with the value being
    written already zero) #GPs on its own `iretq`, with the error code equal to the target selector
    (`G0008 … rip=<the handler's iretq> 0008`), and the guest then re-enters the `#GP` entry about
    1850 times with the stack descending 0x20 per event. It is not the reporter: the same fault
    reaches a *bare* `pop rax; iretq` and still fails. It fits the design — `restore` unmasks the
    timer only around the user `iretq` ("the timer can only fire in user mode"), so the kernel is
    never interrupted and no kernel-mode `iretq` is exercised anywhere; `restore`'s `4:` branch (the
    kernel-mode resume) is therefore untested and this is the first time it has run. Two
    consequences: **any instrument that can trap at CPL0 must not `iretq`** — arm watchpoints only
    where only user code runs (the one-shot arm at the fault before the crash does exactly that) —
    and `restore`'s kernel-mode branch is a latent bug of its own to reconcile with `#PF`'s
    "never iretq, always go through the scheduler" shape.

    *Next, given every copy trap is empty.* The remaining question is the **child's own instruction
    that loads `%xmm0`**, and it needs an instrument that can read the child's memory in the
    fault-free window between the fetch fault at `0x1032000` and the crash, without trapping at
    CPL0. Two candidates: dump the child's live text at `0x1031000..0x1033000` at the crash and diff
    it against the same build's file (to settle whether the operands the file's disassembly names
    are the ones the CPU executed), and dump the child's stack bytes in the window before the crash
    with a QEMU TCG plugin or a `gdbstub` read at the fault (both observe without `iretq`). The
    harness note still stands: the arm's own print, and any per-hit print, must stay proportional to
    *changes*, and a `#DB` counter is quickly eaten by boot page-in traffic.

    *ROOT CAUSE AND FIX (2026-09-22): the kernel was destroying the process's live XMM registers.*
    The corrupt value never travelled through memory at all, which is why every copy trap, every
    page-table scan and every watchpoint on the destination came up empty.

    What happens: `uu_seq`'s `uumain` keeps a value live in `%xmm0` across the page fault at
    `0x1032000` — the 56-byte struct move is `movups 0x2a0(%rsp),%xmm0` / `leaq 0x740(%rsp),%rsi` /
    `movups %xmm0,0x18(%rsi)`, and the fault lands between the load and the store. Nothing in this
    port preserved a user process's FPU/SIMD state across a kernel entry: x86_64 code built by
    rustc uses SSE by default, and the FPU-ownership machinery (`save_fpu`/`restore_fpu`,
    `fpu_owner`, `MiscFlags::FPU_INITIALIZED`) had **no callers at all**, so `%xmm0` came back on
    resume holding whatever the kernel's own last 16-byte copy had left in it. That copy is
    `kernel_call_finish`'s `SYS_VMCTL` reply (`s0 = 0x000000080000062b`, measured), so the first 16
    bytes of VM's kernel-call message landed in `ArgMatches` — the exact shape the earlier sessions
    kept finding and could not attribute, including in a freshly zeroed mmap page and in the
    stack slots clap saved registers to.

    Settled with a data breakpoint armed only for the faulting process's resume (`restore`'s
    user branch, gated on the root recorded when the fault was handled — armed any earlier, VM's
    own work on the fault hits the same VAs and a trap taken at CPL0 cannot be returned from: it
    #GPs in the handler's own `iretq`, which is worth recording separately). With DR0 watching
    `0x0feffd28` for writes and DR1 the disassembly's source slot, the run gives one hit:

    ```
    K: db rip=000000000103200b cs=…1b frsp=000000000feff5d0 dr6=…0ff1 cr3=000000000fc26000
    K: db d0=000000000feffd28 v0=000000080000062b v1=0000000c00008013 d1=000000000feff870 w0=0000000000000004 w1=00000100013a50
    ```

    `v0`/`v1` is the record at the destination; `w0`/`w1` is what the slot the disassembly names
    still holds — the legitimate `{4, 0x1000013a50}` — so the store did **not** take its value from
    there, and after the fix the same store writes exactly `w0`/`w1` into `d0`. That pair of
    readings is the fix's acceptance test.

    Fix: `crates/kernel/src/fpu.rs` saves the live state into `p_seg.fpu_state` (allocating the
    area on first use) as the first Rust on every user→kernel path — `save_fault_context`,
    `syscall_handler_c`, and the timer/serial/keyboard/mouse ISR callbacks — and `restore` reloads
    it with `FXRSTOR` before the user registers (via a `Proc.p_seg.fpu_state` offset registered at
    boot, the same shape `TLS_FS_BASE_OFF` uses). Because the save is on *entry*, a context switch
    in the middle is safe too: each process's state is in its own area before any other process
    runs. `sysretq_to_user` (boot-only, for the first process) needs none.

    Supersedes: the "the writer is a different store in that window" reading (there was no second
    store — the value was in a register), and the whole hop-by-hop plan. It also explains what
    made this look like memory corruption: the process's *own* data is corrupted while every memory
    write is legitimate, so nothing that watches memory can see it.

    Related trap, for whatever instrument comes next: a `#DB`/`#GP` taken at CPL0 cannot be
    resumed by this kernel's `iretq` — the timer is unmasked only in user mode, so no kernel-mode
    `iretq` is exercised anywhere, and the three-entry frame `restore` builds for a kernel resume
    #GPs too (measured; item 6) — so any CPL0-triggered instrument wedges the guest instead of
    reporting.

    Handled with it: `fork` copies the `Proc` struct, which for a *pointer* field means parent and
    child would share one save area and overwrite each other's state — C keeps the FXSAVE area
    inside `Proc`, so the struct copy is a copy there. `fpu::fork_inherit` gives the child its own
    area with a copy of the parent's state (the parent's live state is already saved: fork runs
    inside a syscall), and `fpu::reset` clears it at exec so the replacing image does not start
    with the old image's registers. Thread creation copies fields individually and leaves the
    pointer null, which is what it wants.

    Validated: `just test-coreutils-wedge` green, 5/5 steps (`3 wedge-big.txt`, `200 wedge-big.txt`,
    `WEDGE-OK`) with **zero** `K:`/`G:` text in the log and after the probe was removed;
    `just image-x86` 3/3; `just check` (clippy `-D warnings` + riscv64 kernel check); the
    riscv64/aarch64 kernel builds; `cargo test --workspace`; `just test-qemu x86`; and
    `just test-boot x86` (`ALL TESTS PASSED`) — all re-run after the fork/exec handling above.

    Open follow-up here: the FPU-ownership machinery (`fpu_owner` and `CPULocalStorage`'s accessors,
    `release_fpu`, `used_fpu`, `hw::restore_fpu`) is still vestigial — the fix settles the state
    without an owner, saving on *every* entry instead, so a reader could mistake those for a live
    lazy-FPU scheme. Wiring them up (skipping the save when the FPU is provably untouched) is not
    possible: the clobber is a property of kernel code, not of who last used the FPU. So they should
    be deleted, or kept with a comment saying they are not what preserves SIMD state.

    The rest of what was deliberately left, so none of it is mistaken for an oversight later:

    * **The cost is accepted.** Every kernel entry for a user process now pays an `fxsave`
      (`syscall_handler_c`, `#PF`, and each IRQ callback), a few hundred cycles each. It cannot be
      made lazy (previous bullet). Compiling the kernel with `-C target-feature=-sse,-sse2` was
      tried and measured to be *insufficient*: `core`/`alloc` come from the prebuilt sysroot with
      SSE enabled, so the clobber survives, and it also changes kernel codegen broadly — it was
      reverted.
    * **A future user→kernel path must call `kernel::fpu::save` first.** The hook list, the
      never-twice rule, and the one ISR that exists but is not installed (the profiling clock) are
      in `crates/kernel/src/fpu.rs`'s module comment and in the `bare-metal-debug` skill.
    * **`getmcontext`/`setmcontext` now see real FPU state.** `p_seg.fpu_state` is a valid area
      whenever `MiscFlags::FPU_INITIALIZED` is set, which `save` maintains; before this they always
      saw a null pointer and reported no FPU state. The flag's meaning is unchanged, so nothing
      needs to change for them — but don't "simplify" it away on the assumption nobody sets it.
    * The gate's own comments were stale and are current again (2026-09-23) — they said the recipe
      was red and that the scenario stopped at step 2. The `Justfile` recipe comment and
      `tools/smoke/coreutils-wedge.tsv`'s header now describe the green gate; the recipe itself went
      green with no edit to its steps, which is what it was written to do.

13. **Free slots are left not-quite-free (2026-09-21, open, possibly benign).** An
    exited child's slot is left holding `SLOT_FREE|P_STOP` (`rts=00000041`) or
    `SLOT_FREE|NO_ENDPOINT` (`rts=00000101`), and `Proc::is_empty()` is *equality* with
    `SLOT_FREE`, so such a slot reads as occupied while claiming to be free. `hangdump`
    does not skip it, which made these look like wedged processes: every one of them sat
    at `rip=0x1177dea`, which is `std::process::exit+0x1a` — simply the last instruction
    each had executed. C's `get_free_proc` tests the *bit*
    (`RTS_ISSET(rp, RTS_SLOT_FREE)`), so `is_empty` is the thing to reconcile; whether a
    slot leaks because of it is not established.
14. **A blocking console read parks VFS (2026-09-21, open — measured).** VFS's main loop is a
    single blocking worker: `cdev_io` reaches a driver with `fs_sendrec`, so nothing else VFS owns
    is served while that round trip is outstanding. The tty's console `do_read` then *busy-loops
    inside its handler* until a byte arrives from the serial ring (its own comment: "retry in user
    mode"), so a shell waiting at its prompt for a line holds VFS for as long as the user types
    nothing. Measured with a trace of VFS's sendrecs: `K: set sendrec dst mtype 1 5 402`
    (CDEV_READ to the tty), `REPLY_PEND` set, and VM's FDIO for a running child's page deferred
    behind it — the child stays `PAGEFAULT`-blocked and its page-in is never issued until input
    arrives. The pty path avoids exactly this ("a suspended read would freeze VFS"); the console
    path does not. A backgrounded `coreutils seq` therefore stalls at its first page-in, which is
    why the background form of the wedge looks different from the foreground one. After the kill
    fix, the stall still measures the same way but sits elsewhere: a backgrounded `coreutils factor
    60 &`, 30 s and one typed line later, shows the child `PAGEFAULT`-blocked at its ELF entry
    (`ep=00008013 rts=00000400 mf=00006001 rip=0x1000000`) with VFS blocked in a `SENDREC` to MFS
    (`GETFROM=00000007 mf=1`) while MFS is runnable, so the page-in is still in flight, not lost —
    the request that would end the fault has not come back. Which round trip is stuck (the
    console read, or the VFS→MFS read behind it) is not yet pinned. Fixing the class means either a
    console read that returns EAGAIN to a retrying reader (the shell's `read_line` already retries
    on EAGAIN, `shell.rs`) or a real suspended-request path in VFS (`FP_BLOCKED_ON_CDEV` +
    `cdev_reply`, still a stub — item 7).
15. **`write_to_proc`/`read_from_proc` silently ignore their `proc` argument (2026-09-22, FIXED for
    these two; `vm_memset`'s non-seam path still open).**
    Both take a process number and a virtual address, but where the page tables are the kernel's to
    walk (`CROSS_ADDRESS_SPACE_COPY` is `None` — x86_64, riscv64, aarch64; every hardware port)
    they reduce to a bare `copy_nonoverlapping` through whatever address space is currently
    loaded, so the copy happens in the *running* process's space at `addr` and `proc` participates
    only on wasm. The precondition is therefore "the target is the current process", and nothing
    says so: a caller that passes a third process's number gets no error, just a silent write into
    the running process's memory at `addr` (or a kernel-mode fault where that address is
    unmapped). Callers today are the exec-frame read (`do_exec_load_handler`), the grant-table
    read in `grants.rs`'s `verify_grant` (the granter's `s_grant_pa` is physical, so it works only
    because the identity map reaches it), and the devio/getmcontext/setmcontext copies, which
    switch CR3 to the caller first — correct since the 2026-09-22 CR3-restore fix, and correct
    *because* of it. Same family as that fix: a copy that is right only while the CPU happens to
    be on the target's root, where being wrong means kernel data lands in some process's memory.
    Fix by switching CR3 to `proc` for the copy as `delivermsg`/`virtual_copy` do, or by making
    the "must be current" precondition explicit in the name and an assertion.

    **Fixed in that direction (2026-09-22).** Both functions now read the target's `p_seg.p_cr3`
    and, when it differs from the loaded root, switch for the copy and restore the caller's — the
    shape `delivermsg` uses. It is a no-op wherever the caller already switched (devio, the
    mcontext pair, the console read at `syscall.rs`, all of which pass `(*caller).p_nr`), and it
    makes `grants.rs`'s `verify_grant` behave the way its own comment says C does ("`data_copy`,
    which switches to the granter's page table"). Guarded on `boot_cr3() != 0`, so host tests and
    pre-init keep the bare copy. Not covered: `vm_memset`'s non-seam path
    (`kernel/src/vm.rs`, the `write_bytes` below the seam branch) still writes through the current
    root while two of its three callers pass *another* process's number (`do_setgrant_handler`,
    `system.rs`'s memset handler) — the same defect, and it should take the same switch.

16. **A new process's root maps low physical memory with *user* access (2026-09-22, open —
    unaudited).** `exec_elf_for_target` builds the fresh root with `exec_create_root`, whose own
    comment says it copies "the identity map with **user access** in the low window". If that window
    is user-readable, and especially if it is user-*writable*, then any process can reach low physical
    memory — other processes' frames and kernel data among it — at `VA == PA`, with no kernel-mediated
    copy involved at all. That is the one channel by which bytes could move between two processes
    without passing through `virtual_copy`/`delivermsg`/`kernel_call_finish`/the grant paths, all of
    which are trapped empty for the wedge's corrupt value (item 12). It is therefore worth excluding
    on its own rather than assuming it is safe: `VM_PAGING_CLEAR`'s comment ("the supervisor identity
    map above 1 GiB") suggests the window is deliberate and bounded, but where the bound falls, and
    whether the leaves are writable, is not established. Measure: walk a fresh root for a low physical
    page and read the U/S and RW bits, and check what fraction of the frame allocator's range the
    window covers.

    *Working tree, pending review (2026-09-22):* `exec_create_root` (`arch-x86_64/src/hal.rs`),
    `boot_create_restricted_page_table` (`kernel-boot/src/boot_init.rs`) and
    `exec_setup_new_page_table` (`kernel/src/exec.rs`) now clear `PG_U` on every copied identity
    entry, which is what this item asks for; that leaves the measurement above relevant only for
    confirming no other low-window mapping still carries user access.
17. **The kernel's `brk` handler keeps one global `CURRENT_BRK` for a per-process quantity
    (2026-09-22, open — possibly vestigial).** `syscall.rs`'s `CURRENT_BRK` is a process-global
    `AtomicU64` used by `sys_brk_handler` (NR_BRK, registered as basic syscall 36) for both the query
    and the update, so two processes growing their heap share one break and each sees the other's.
    The port's userland goes through VM's `VM_BRK` instead (`do_brk` tracks `vm_region_top` per
    `Vmproc`, and `.rules` says VM owns `brk`), so the kernel copy may be vestigial — but if anyone
    calls it, it reports another process's break. Establish which of the two the port trusts; if the
    handler is reachable, a `Proc`-resident break (or removal of the handler) is the fix.
18. **Two physical page allocators are initialised over the same RAM (2026-09-22, latent).**
    `kernel-boot/src/main.rs` feeds the *same* range to both: `arch_x86_64::alloc::init_allocator`
    (the arch bitmap, lines ~304) and `kernel::vm::mem_init` (the MINIX bitmap, ~356) each get
    `[kernel_end, mem_top)` minus the user stack/brk windows, so each believes the whole pool is free.
    Nothing double-allocates today only because `vm::alloc_mem`'s callers are the test-only
    `exec_setup_new_page_table` and the boot loader — but the comment claiming its purpose is
    "used by kernel call 62 (VM_PAGING_ALLOC)" is stale: `do_vm_paging_handler` allocates from
    `hal::alloc_phys_contig` (the arch allocator), which is also what `exec_elf_for_target` and
    `boot_init` use. So the MINIX bitmap is effectively dead weight that must stay unused; anything
    that starts calling `vm::alloc_mem` at run time hands out frames the arch allocator has already
    handed to a process, with no ownership either side can see. Fix by initialising one allocator
    (or by giving the two disjoint chunks), and delete the stale comment.
19. **A lookup path of 24 bytes or more killed VFS (2026-09-23, FIXED).**
    `req_lookup` built the pathname into the 56-byte request message and wrote the NUL
    terminator at `PAYLOAD_OFF + 24 + path_copy_len`. The guard around that write tested
    `24 + path_copy_len < 56`, which never fires (the copy is itself capped at 24), so a
    path of **exactly 24 bytes** wrote byte 56 of a 56-byte array: VFS panicked
    (`crates/servers/src/vfs/request.rs:705`) and the filesystem server died, taking the
    shell with it — the guest simply stopped answering. Longer paths were worse in a
    quieter way: the message advertised a `path_len` it did not carry and every FS
    parser truncated to 24 bytes, so a path past 24 was resolved as its first 24 bytes.
    Reproduced with `cat` on a 24-character name and bracketed in the guest: 23 bytes
    resolved, 24 panicked, every time.

    What it was blocking: `coreutils pr` and `ptx`, excluded from `feat_minix` as
    "guest-side memory corruption" that "wants a kernel/VM look". It was never pr's bug
    and not item 12's XMM clobber — `pr` calls `metadata()` and formats a timestamp, and
    jiff's system timezone probes a `/usr/share/zoneinfo/...` path longer than 24 bytes.
    (The `U` bytes `sort` printed and the clap TypeId that is not a hash *were* item 12.)

    Fixed by sending the path in a **direct grant** on VFS's own `l_path`, which is what
    the C's `req_lookup` does (`cpf_grant_direct`, `CPF_READ`): the message carries
    `path_len` at payload[20..24] and the grant id at payload[24..28] instead of path
    bytes. Every parser moved with it — MFS's dispatch and `fs_lookup`, `ext2/path.rs`,
    and `libs::vtreefs` — because a writer/parser mismatch here is silent; that is what
    an earlier grant-based `mknod` layout did (it created nothing). `path_len` is the
    byte count without the terminator, one convention for the whole port: the C sends
    `strlen + 1` plus a separate `path_size`, and each FS here adds its own NUL.

    Verified in an x86 guest: `echo >`/`cat` on names of 16, 20, 21, 22, 23 and **24**
    bytes all work, and the gate that covers them (`just test-long-path`, which gives
    every read-back its own marker) is green; `pr -t`, `pr`, `ptx` and `ptx -r` produce
    real output; `just image-x86` 3/3. The writer's layout is pinned by
    `lookup_request_layout_is_what_the_fs_parsers_read` in `servers`, and the host
    drivers were updated to match (`crates/fs/tests/ext2_image.rs`, vtreefs's test).

    One correction to how this was proved: the first version of the scenario expected
    the same short string in every step, so its later steps were satisfied by an earlier
    step's output and a real failure went unseen — item 20's create limit "passed" that
    way at 30 bytes. It is the trap `silent-failure-traps` describes, met while writing
    the gate, and giving each step a distinct marker is what exposed item 20.
20. **A name longer than 28 bytes cannot be created (2026-09-23, open — measured).**
    `create`, `mkdir` and `mknod` carry their single name *inside* the request, so the
    name is capped by the room left after the fixed fields: 28 bytes for
    `req_create`/`req_mkdir`, 29 for `build_mknod_msg` (a 30-byte name wrote byte 56 of
    56 as well; that write is bounded now), while `path_len` still advertises the
    untruncated length. Measured in an x86 guest with `echo n > <name>`: names of 24,
    25, 27 and **28** bytes create and read back; **29**, 30 and 32 bytes fail with
    `sh: cannot create <name>: err=02` (ENOENT) and leave nothing behind — that is the
    shell's own `>` redirect, so this is not a coreutils-only path. It fails loudly
    rather than creating a file under a truncated name, which is the better half of the
    news; `MFS_NAME_MAX` is 60, so 29-60 bytes is still a real gap for anything that
    creates files. `coreutils mkdir`, `ln`, `truncate` and `split` are all in
    `feat_minix`.
    The fix is the one lookup just got — grant the name — and it has to land on both
    sides at once: `parse_mknod_request`'s test in `crates/fs/src/mfs/main.rs` exists
    because an earlier grant-based layout was never parsed and `mknod` created nothing.
    Its gate belongs in `tools/smoke/long-path.tsv` (whose header says why no step goes
    above the 28-byte create limit) or beside it.
    Measured with a scratch scenario, `target/tmp/create-boundary.tsv`.
21. **~~The environment did not survive `exec`~~ — FIXED (2026-09-24).**
    Every process started with an empty environment whatever its parent passed:
    the chain dropped `envp` at all three of its Rust-side links. The C
    `execve` took `_envp` and ignored it, `minix_std::process::exec` had no
    environment parameter and handed the frame builder a null, and `getenv` was
    a stub that returned NULL — while `minix_rt::execve` and the kernel's frame
    *parser* already carried `envp`, so half of it was built and nothing used
    it. Now the C `execve` counts `envp` (capped at 63, which is what the frame
    holds), `minix_std::process::exec` takes and forwards it, `crt0` publishes
    the block as `environ` through `__minix_set_environ` before `main` (so a
    constructor can `getenv`, and `environ` is an empty array rather than null
    before that), and `getenv` walks the block. Measured in an x86 guest:
    `tools/ctest.c` re-execs itself with `CTESTENV=hello` and the child prints
    `getenv: hello`; before the fix the same step printed `getenv: unset`. This
    is what bash needs for `PATH`, `HOME` and `TERM` and for its children.
    (`crates/minix-libc/src/c_sys.rs`, `crates/minix-std/src/process.rs`,
    `crates/minix-libc/src/c_stdlib.rs`, `tools/crt0-x86_64.S`)

22. **~~The shell did not remove quotes~~ — FIXED (2026-09-24).**
    `crates/userland/src/shell.rs` split a command line with
    `split_whitespace()`, so a quoted argument reached its program as
    fragments: `bash -c 'echo BASH-OK'` handed bash `'echo` and `BASH-OK'`, and
    bash's reply — ``BASH-OK': -c: line 1: unexpected EOF while looking for
    matching `''` — reads as a defect in bash rather than in the shell that fed
    it. Nothing in the tree had used a quote before, which is why it survived:
    every smoke scenario and every `MINIXFS_EXTRA` probe is quote-free. The
    tokenizer is quote-aware now (`shell.rs::tokenize`): `'…'` and `"…"` are
    stripped in place, whitespace inside them stays in its word, a backslash
    escapes the byte after it, `echo ''` still passes one empty argument, and an
    unclosed quote is refused (`sh: unexpected EOF while looking for matching
    `'`) instead of being run with the quote as text. Measured in an x86 guest:
    `/bin/bash -c 'echo BASH-OK'` prints `BASH-OK`; before, it printed the
    unmatched-quote error. Unit tests (`shell::tests::tokenize_*`) cover the
    cases the guest cannot reach cheaply.
23. **~~`getcwd` was a stub returning `ENOSYS`~~ — FIXED (2026-09-24).**
    VFS holds a directory's *vnode*, not its name, so the name has to be
    recovered by walking up: `stat(".")` and `stat("..")` identify the
    directory, a scan of `".."` finds which entry holds that inode, and
    `chdir("..")` moves up for the next round — root is where the two stats
    agree. That is MINIX's own `__getcwd`
    (`minix/lib/libc/sys/__getcwd.c`) and the port now ports it, including its
    duty to restore the caller: the walk ends at the root, so the components it
    found are re-descended before returning, on the failure paths as well as the
    success one. The allocate form `getcwd(NULL, size)` is implemented too,
    because that is how bash asks (`getcwd(0, PATH_MAX)`,
    `bash/builtins/common.c`). Measured in an x86 guest from `/tmp/cwdtest`:
    `getcwd=/tmp/cwdtest errno=0`, `getcwd(NULL,0)=/tmp/cwdtest`, and a
    relative-path write issued *after* the call lands in `/tmp/cwdtest` — which
    is the restore duty, not a formality. bash's startup then reports a cwd of
    its own: `/bin/bash -c 'printf "PWD=%s\n" "$PWD"'` prints
    `PWD=/tmp/cwdtest`, and the `shell-init: error retrieving current directory`
    line is gone.
    The failure's *name* was a second gap in the same path: `strerror`'s table
    stopped at 34, so `strerror(ENOSYS)` (78 here) said "Unknown error" and the
    real message was `getcwd: cannot access parent directories: Unknown error`.
    The table now covers every errno `tools/c-include/errno.h` declares, with a
    host test that reads the header and asserts it
    (`c_string::tests::every_declared_errno_has_a_message`) and a second that
    holds every message inside `strerror`'s buffer, which was 32 bytes and
    silently truncated the long ones.
    (`crates/minix-libc/src/c_sys.rs`, `crates/minix-libc/src/c_string.rs`)
24. **`std::env::current_dir` has no implementation for minix (2026-09-24,
    open).** `rust/library/std/src/sys/paths/mod.rs` routes `target_os =
    "minix"` to `sys::paths::unsupported`, so `current_dir` returns
    "unsupported operation" whatever the libc can do — it does not call
    `getcwd`, which is why item 23 does not fix it. `std::env::current_dir`,
    `std::fs::canonicalize` and `Command::current_dir` all go through it, so
    `coreutils pwd` fails and any applet that resolves a relative path in Rust
    rather than leaving it to VFS is wrong. The fix is to route minix to
    `sys/paths/unix.rs::getcwd` (which calls `libc::getcwd` and grows on
    `ERANGE`); it needs the Rust fork and a stage1 rebuild, so it is not a change
    to make alongside libc work.
25. **~~MFS `getdents` returned `OK` at end-of-directory without a reply payload~~
    — FIXED (2026-09-24).** The end-of-directory path returned a bare `0`,
    which the VFS reads as `OK`, and then took `nbytes` from a payload that path
    never wrote — the *previous* call's byte count. A reader looping until
    `getdents` reports 0 (the libc's `readdir`, and `ls` now that it reads a
    whole directory to sort it) was therefore handed the same entries again for
    ever: `ls` in an x86 guest printed nothing and never returned to a prompt.
    The old single-call `ls` never reached the end path, and `readdir` stopped
    as soon as it found the name it wanted, which is why neither had shown it.
    The path now replies with `seek_pos` unchanged and `nbytes = 0`. Measured:
    `ls` of a 41-entry `/bin` lists it in full, and `ls | cat` terminates.
    (`crates/fs/src/mfs/protect.rs`)
26. **`isatty` answers 1 for any of fd 0..2 (2026-09-24, open).**
    `crates/minix-libc/src/c_sys.rs` decides "is this a terminal" by the fd
    *number* — `if (0..=2).contains(&fd) { 1 } else { 0 }` — so a C program
    redirected into a file or a pipe is still told it is on a terminal. bash
    uses exactly that to decide whether to be interactive, and coreutils tools
    use it for colour and piping behaviour, so this is not cosmetic. The real
    test is available and now used by `ls` (`crates/userland/src/lib.rs`):
    `fstat` and check `S_IFCHR` — the console's descriptors are character
    devices, a pipe's vnode is `S_IFIFO` (PFS creates it `I_NAMED_PIPE`) and a
    file's is `S_IFREG`. The ioctl is *not* a test: VFS answers an ioctl it does
    not route with `OK`, which is how `ls | cat` laid out columns before the
    check above existed. (`crates/minix-libc/src/c_sys.rs`)
27. **A fork shared the parent's writable pages with the child, so the parent's own writes
    reached the child (2026-09-24, FIXED).** `vm_paging_fork` clears `PG_RW` on the child's
    writable user leaves and leaves the parent's alone, so after a fork the parent still had
    write access to the very frames the child aliased. Every write the parent made between the
    fork and the child's first store to a page was therefore visible to the child — the fork was
    not the snapshot `fork(2)` promises, in the direction nobody tested (`/bin/forktest` only
    checked that the *child's* write did not reach the parent).

    *Found through bash (see `C_BUILD.md`).* A command that forks — `cat /etc/passwd`, and any
    other external command — killed the child with a ring-3 `#GP` at `list_length`
    (`G0000 0000000001084590 001B …`). The fault-time register file, read with a gdbstub
    breakpoint on `exception_gpf_entry` (`target/tmp/bash-gpf-probe.py`), gave
    `rax=0xdfdfdfdfdfdfdfdf` — non-canonical, and the operand of the faulting
    `mov (%rax),%rax`. `0xdf` is bash's own poison: `ocache_free` (`bash/include/ocache.h`)
    `OC_MEMSET`s a freed word list with it. `execute_simple_command` runs `dispose_words (words)`
    in the *parent* right after `execute_disk_command`'s fork (`execute_cmd.c:4945`), and the
    child's first use of that list is `list_length` inside `strvec_from_word_list`, so the child
    read the parent's scrub as a `next` pointer. The child had never written the block, which is
    why the sibling page it did write (`/bin/forktest`'s) never saw it: a store gets a private
    copy either way, and only a *read* of an untouched page shows the leak.

    *Fixed* in VM: `cow_setup_fork` now walks the same leaves and, for each page carrying the
    kernel's fork marker that is not in a `VR_SHARED` region, gives the child a private copy
    (`vm_alloc_pages` + `vm_copy_pages` + `vm_map_page_in`) instead of aliasing the frame.
    Genuinely shared pages (MAP_SHARED file pages, whose frame is the file cache's) keep the
    alias and their PhysBlock reference, which is what `handle_cow_fault`'s `VR_SHARED` branch
    re-enables writability on. Nothing is left read-only for the parent, so no kernel-side write
    (`sys_vircopy`, `write_to_proc`, `delivermsg`) can fault on a COW page — that is the trade:
    the reference's both-sides COW needs the `vm_suspend`/`VMSUSPEND` path (MINIX
    `arch/i386/memory.c:virtual_copy_f`), which suspends the caller, hands VM the *target* of the
    copy, and restarts the kernel call; this port has no such path, and a kernel copy that hit a
    read-only user page would be blamed on the running server (`handle_page_fault` uses
    `current_proc()`) and kill it. Deferred, not lost.

    *Covered* by `/bin/forktest`, which now also checks the direction that was missing: a page
    the child only reads must keep its fork-time contents while the parent writes it. Verified
    both ways — green with the copy, and with the copy disabled as a negative control it reports
    `forktest: parent's write reached the child (fork not a snapshot)` and exits 6.
    (`crates/servers/src/vm/cow.rs`, `crates/servers/src/vm/mod.rs`,
    `crates/userland/src/bin/forktest.rs`)

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
4. ~~`[toolchain]` **virtio-blk reads fail after the 2026-09-17 fork rebase**~~
   — **FIXED**: RISC-V only; x86 and aarch64 were green. The transport
   overloaded bit 0 of `VirtioPhysBuf::addr` as the device-writable flag
   (`vd.addr = (vp.addr & !1) + phys_delta()`), which silently rounds an odd
   buffer address down by one byte. The blk driver's 1-byte `STATUS` static is
   byte-aligned, so its address parity depends on `.bss` layout: it landed at
   `0x100e001` (odd) on RISC-V after the compiler bump, but at an even address
   on x86 (`0x100f018`) and aarch64 (`0x100d000`), which is why only RISC-V
   broke. The device completed the read correctly and wrote `VIRTIO_BLK_S_OK`
   to `...c000` while the driver polled `...c001`, so `wait_for_completion`
   always timed out (`read failed err=Unknown`); MFS's cache stayed zeroed
   (`read_super: magic=0 blk=0`), `mount_root()` returned null, and the boot
   suite reported 3 failures. Fixed by giving `VirtioPhysBuf` an explicit
   `writable: bool`. The same latent hazard covered virtio-input event
   buffers (odd addresses inside `[[u8; 8]]`) and any other odd-addressed DMA
   buffer. Diagnosed without guest-side instrumentation via the host QMP probe
   (`tools/riscv_blk_probe.py`), which reads the vring and DMA buffers out of
   guest physical memory. The `mount_devman` null guard in
   `crates/servers/src/vfs/main.rs` keeps a future mount failure from turning
   into the VM livelock described here.
5. **The register file has two byte layouts, and the HAL named the wrong one
   — FIXED (2026-09).** `p_reg` is *not* the trap frame. The trap entry saves
   x0..x31 at 0..248 with `sepc` at 256, `sstatus` at 264 and `scause` at 272
   (`trap_asm.rs`, a 296-byte frame); `p_reg` keeps `sepc` in x0's slot, x1..x30
   at 8..240, and `sstatus` in x31's slot, with the real `t6` in `Proc::p_t6` —
   `t6` need not survive a trap, because `switch_to_user` uses the register and
   the psABI lets a syscall clobber a caller-saved temp. The post-syscall hook
   (`kernel-boot/src/riscv64.rs`) translates between them and documents both.
   Three HAL functions were written against the wrong one or not at all:
   `write_frame_ip` was `todo!()` (a panic the first time sepc had to be moved),
   the `mcontext` pair was `todo!()`, and `bkl_lock`/`bkl_unlock` were `todo!()`.
   `Mcontext` gains `from_frame`/`write_into_frame` and the conversion now lives
   there, where the host can test it (`hal` is `target_arch`-gated, so anything
   tested in it never runs — see the testing notes); `bkl` is an empty no-op,
   which is what `BKL_LOCK` is on every other arch while SMP is off
   (`arch-x86_64/src/spinlock.rs` compiles the primitives out).

---

## AArch64 (`[aarch64]`)

1. **Fork is a proper COW fork (was deep copy).** x86/riscv fork by clearing
   the child's write bit; aarch64 does the same, with the access encoding expressed
   through HAL helpers (`pte_is_writable`/`pte_set_writable`/`pte_is_user`)
   because AP[2:1] is a 2-bit field, not a single RW bit. That cleared bit is the
   fork *marker* VM reads: `cow_setup_fork` then gives the child a private copy of
   each marked page, keeping the alias only for MAP_SHARED frames (item 27 — the
   parent's PTEs used to stay writable with the frame shared, so the parent's own
   writes reached the child). The shared low-GB alias leaves stay verbatim
   (`alloc::is_alias_frame` — never copied, never COW'd). VM's
   `cow_setup_fork` + the COW message-buffer prefault are active. Verified
   by `/bin/forktest` (fork + write isolation, both directions) on all three arches
   and the flat exec loop. (`crates/arch-aarch64/src/fork.rs`, `crates/servers/src/vm/cow.rs`)
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
7. **Boot stack vs. physical allocator overlap — FIXED (2026-09).**
   `tools/minix-raw-aarch64.ld` reserved the 64 KiB boot stack *before*
   `__kernel_end`, and `kmain` starts the physical allocator at `__kernel_end`,
   so the loader allocated pages inside its own running stack. Latent since the
   script was written; an image grown by ~60 KiB armed it, and it presented as an
   endless loop in `load_and_prepare_proc` with **no** serial output (a corrupt
   loop counter: 3 segments logged, 4 in the disassembly). The stack reservation
   now sits between the image and `__kernel_end`, and
   `test_allocator_aarch64_clears_boot_stack` compares the allocator's *base*
   against `__stack_top` rather than sampling a fresh allocation (a sample passes
   in both layouts, because by then the loader has moved the free cursor). Any
   change that grows the aarch64 image is a candidate to re-arm a latent overlap
   of this shape — see `PORTING_PLAN.md` finding 19.
8. **`trapframe_to_mcontext` returned a zeroed context — FIXED (2026-09).**
   The pair was `Mcontext::default()` and an empty function, so
   `SYS_GETMCONTEXT` would have reported an all-zero register file rather than
   failing. AArch64 has a single frame layout — `p_reg` *is* the exception frame
   (x0..x30 at 0..240, `SP_EL0` at 248, `ELR_EL1` at 256, `SPSR_EL1` at 264;
   `set_initial_regs`, `switch_to_user` and the post-syscall hook all agree) — so
   the conversion is a straight register-file copy plus those three words, now in
   `Mcontext::from_frame`/`write_into_frame` where the host can test it.
   `tpidr_el0` is not in the frame at all: the kernel keeps it in `Proc::p_tls`
   and reloads it on every switch.

---

## Testing / tooling notes (`[all]`)

- `exec_loop_mem.py` now takes an `ARCH` argument (x86 | riscv64 | aarch64)
  with per-arch QEMU invocation; aarch64/riscv64 must not use
  `-monitor none` (it drops UART input bytes there).
- **Tests in `arch-riscv64`/`arch-aarch64`'s `hal.rs` never run.** Their `lib.rs`
  gates `pub mod hal` on `target_arch`, and the host is x86_64, so those modules
  are not compiled by `cargo test` at all — and the QEMU builds do not build
  tests. x86_64's `hal` *is* host-compiled, which is why its
  `trapframe_mcontext_roundtrip_preserves_regs` does run and hides the asymmetry.
  Put anything that must be verified in a portable module (`frame.rs`,
  `mcontext.rs`, `psl.rs`, …) and let `hal` delegate to it; verify the target-only
  code by running the arch's QEMU gate.
- `/bin/forktest` (userland bin, `crates/userland/src/bin/forktest.rs`) is
  the fork isolation test: two 4 KiB writable `.data` pages (separately aligned,
  so a store to one cannot drag the other across the fork) are filled, forked, and
  both sides checked. The child writes `PAGE` and verifies its own write; the
  parent verifies the child's write did not land in its view. Then the parent
  writes `WATCH` — which the child only ever *reads* — and the child, after a spin
  long enough for a timer quantum to land in it, verifies it still reads the
  fork-time contents. That last step is the one that catches a fork whose pages
  are shared rather than copied: a page the child writes gets a private copy
  either way, so only a read of an untouched page shows the parent's post-fork
  write reaching the child (item 27). It runs as a normal command (shell fork →
  exec); for image injection use `MINIXFS_EXTRA=/bin/forktest=...`.
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
- **`coreutils/build.rs` must not declare a `rerun-if-changed` path it does
  not have (2026-09-24, patched in the submodule).** It emitted
  `cargo:rerun-if-changed=docs/tldr.zip` unconditionally, and that archive is
  in `docs/.gitignore` — a download, read only by the `uudoc` binary we do not
  build — so the path is *missing* in every checkout, and cargo treats a
  declared-but-missing path as always changed. The build script therefore
  re-ran on every build, rewrote `uutils_map.rs`, and dirtied the crate through
  `StaleDepFingerprint`: `Compiling coreutils` (~30-48 s) on every
  `just build-x86`, with no input changing. Cargo names the reason itself under
  `CARGO_LOG=cargo::core::compiler::fingerprint=info`, which is how it was
  found: `dirty: FsStatusOutdated(StaleItem(MissingFile { path:
  "…/docs/tldr.zip" }))`. The `println!` is inside an `exists()` guard now, and
  `just build-x86` went from ~50 s to ~5 s. **It is a patch to the `coreutils`
  submodule, so a rebase onto upstream drops it** and the ~45 s per build
  returns silently; re-apply it there, and use that `CARGO_LOG` line to tell
  "the build is slow" from "something is being rebuilt that should not be".
- `[env]` **MSYS converts a POSIX-style `MINIXFS_EXTRA` value on the way to a
  native tool, and the file lands in `/` instead of `/bin` (2026-09-24).** The
  exclusion used to be only on the `mkfs-*` lines, and those do not read the
  variable — it is read by the *kernel* build script, reached through `build-*`.
  So `$env:MINIXFS_EXTRA='/bin/bash=…'; just run` produced `/bash`: the
  converted dest (`C:/Program Files/Git/bin/bash`) fails `starts_with("/bin/")`
  and `boot-image::minixfs` routes anything unrecognised to the root,
  *silently* — the file's data and a well-formed dirent both existed, in the
  wrong directory, and `ls /bin` simply did not list it. Both halves are fixed:
  the Justfile exports `MSYS2_ENV_CONV_EXCL` for every recipe, and
  `crates/kernel/build.rs` refuses a dest that is not `/bin/`, `/sbin/` or
  `/etc/`, naming the exclusion in its message.

  It has to be an `export` and not a per-line prefix, which is the second half
  of the trap: `just run` re-enters `just` (`run` → `run-x86`), and the value is
  converted when the *inner* `just.exe` is started by the outer recipe's shell —
  before any line of the inner recipe runs, so a prefix on the line that starts
  `target/mkboot` is too late. Measured: with such prefixes in place, a `just run`
  from PowerShell still panicked in `build-x86`; with the export, the same
  `just image` (the same nesting) builds and boots. Verified: the dirent is in
  zone 7 (`/bin`) and `/bin/bash --version` answers, where before it was in
  zone 6 (`/`) and only `/bash --version` did. The `mkfs-*` lines had carried an
  exclusion prefix from the start; those are removed, because `tools/mkfs.rs`
  reads no environment at all — it copies the assembled image — so the prefix
  never did anything and its comment described the wrong half of the build.
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
- **The bare-metal suite carried an arch assumption that only a bare-metal run
  can see.** `syscall_brk` in `crates/kernel/src/tests.rs` asserted x86_64's
  heap base as a literal (`0x3FE00000`, which riscv64 also uses and aarch64 does
  not — it is at `0x2000_0000`), so `just test-qemu-aarch64` failed at
  `FAIL syscall_brk` while `cargo test -p kernel` stayed green. It stayed green
  because `tests.rs` is behind `kernel`'s `qemu-tests` feature and the host suite
  does not enable it: these tests are compiled by a feature the host build never
  turns on, so a hardcoded address there is invisible until `just test-qemu-<arch>`
  runs. `syscall.rs`'s own brk tests had already been made arch-neutral when the
  handler was; these had not. Now expressed as `hal::user_heap_base()` plus
  `syscall::BRK_WINDOW_SIZE`, so nothing in the suite names an address.
  Worth generalising: the suite is the *only* coverage for several of its own
  tests, so a change to one arch's VA layout has to be followed by all three
  `test-qemu-<arch>` runs, not by the host suite.
- **`just test-coreutils-wedge` is green as of 2026-09-22, and it stays the
  acceptance test for item 12** (allocation-heavy coreutils tools must write
  their output and the next tool must read it back). It was written to go green
  with no edit, and did: the last of item 12 was the kernel clobbering a
  process's live XMM registers, fixed in `crates/kernel/src/fpu.rs`.
- **The gate's prose was stale and is current again (2026-09-23).** The
  `Justfile` recipe comment said "Red as of 2026-09-21" and explained that only
  step 1 passed; `tools/smoke/coreutils-wedge.tsv`'s header said it was "red
  until the cause is fixed" and stopped at step 2. Both are comments, so nothing
  depended on their text — only the recipe's steps are asserted — and both were
  rewritten to describe the green gate while keeping the two rules that still
  matter: every step carries a tab separator, and its readback is a whole line no
  other step can print.
- **`cargo clippy` on the host does not compile `#[cfg(target_os = "minix")]` code.** A new
  `dangerous_implicit_autorefs` error reached through a raw-pointer field in `mfs/path.rs`
  passed the whole workspace clippy cleanly and only appeared when `just image-x86` reached
  the `userland-x86` build. `just check` (host clippy plus a riscv64 *kernel* check) does not
  cover it. Anything touching a server's minix-only path needs a minix-target build —
  `just build-x86` — before a clippy result means anything.
- **A run of that gate must have zero `K:`/`G:` text in its log.** Every
  instrument this chase used printed through the kernel's serial path
  (`K: …` for a probe, `G` for the `#GP` handler), and a probe left in place
  both perturbs the guest and hides a wedge gate that would otherwise pass —
  the rule in this repo is that probes are stripped and the gate re-run before
  hand-back, not that the gate is judged with the probe still in.
- **~~A target C compile is not hermetic: the host's headers answer for
  minix~~ — FIXED (2026-09-24).** `clang --target=x86_64-unknown-none -I
  tools/c-include` also searched `/usr/include`, so any header the port lacked
  was satisfied by glibc and any *description* of it was glibc's: configure
  reported `HAVE_UNION_WAIT`, `HAVE_TERMIOS_H` and `HAVE_STRINGS_H` yes for a
  target that has none of them (the port does ship a `sys/wait.h`, but without
  `union wait`), and bash compiled paths no real system has. It surfaced as
  `locale.h` being rejected: `HAVE_STRINGS_H` came from
  `/usr/include/strings.h`, which pulls `bits/types/locale_t.h`, which collided
  with the port's own `locale_t`. The flags — `-nostdinc -isystem
  <clang resource dir>/include -I tools/c-include` — now live in
  `tools/ccflags.py`, shared by `tools/build-c-hello.py` and the bash `cc`. A
  missing header is a hard error now, which is the point. What it fixed, in
  order: `_POSIX_VERSION` had to exist for bash to stop using `union wait`;
  `HAVE_TERMIOS_H` answers no; `clock_t` needed a `sys/times.h` to be found
  (bash writes `#define clock_t long` otherwise, which collides with the host's
  typedef in the build tools); and `locale_utf8locale` needed `wcwidth`, without
  which `HANDLE_MULTIBYTE` is off while bash's globbing still uses an identifier
  only the multibyte branch declares. `C_BUILD.md` has the write-up.
  (`tools/ccflags.py`, `tools/build-c-hello.py`)
- **What the hermetic compile then exposed: the port's own missing headers.**
  With the host out of the picture, the bash build's remaining errors are a
  to-do list rather than a mystery: `sgtty.h` in 5 files (bash's terminal
  handling falls back to sgtty because there is no `termios.h`, which is the
  next real piece and is kernel-facing), `sys/ioctl.h` (window size),
  `sys/param.h` (`MAXPATHLEN`), and `mktemp`/`mknod` (absent from the libc).
  `netopen.c`'s `_`/`internal_error` is a different shape — NLS-disabled gettext
  — and is not diagnosed. (`tools/c-include`, `crates/minix-libc`)
- **The C headers are generated and checked, and both sides agree.**
  `tools/gen-c-headers.py` derives the headers from the libc with cbindgen into
  `target/c-include/`; `tools/check-c-headers.py` asserts the two sides agree and
  now reports 0 in each direction: 363 exports, 359 declarations.
  The checker had been under-reporting in three ways, all fixed: a prototype
  wrapped over two lines was not a declaration to it (`pthread_create`, `qsort`,
  `sendto` and a dozen more read as undeclared while sitting in the headers); a
  struct member ended in `;` and did read as one (`d_ino`, `pw_name`,
  `sa_family`, `ru_utime`, …); and tracking bodies swallowed each header's
  content at its `extern "C" {`. The eleven exports it then reported missing were
  real — `issetugid`, `setegid`, `seteuid`, `setgroups`, `logb`, `pthread_kill`,
  `utime`, `utimes`, `vsscanf`, `vfscanf`, `vscanf` — and are declared now
  (`utime.h` and `sys/times.h` are new headers). `bsearch` was the one *declared*
  function with no implementation, i.e. a link error waiting for the first C
  caller, and is implemented. Retiring `tools/c-include` is still a merge rather
  than a deletion: cbindgen derives declarations but not constants (`EOF`,
  `SEEK_SET`, the errno numbers, struct layouts), so each hand-authored header
  has to keep its types and macros and take the generated declarations, after
  which the check reads the set the compiler actually gets.
  (`tools/gen-c-headers.py`, `tools/check-c-headers.py`)
- **`/bin/ctest` is embedded but no gate drives it.** `tools/ctest.c` is the C
  smoke test (errno, the malloc family, stdio, strings, pthreads, the `scanf`
  family — the only on-target call of those variadic entry points anywhere in the
  tree — `mkfifo` through the VFS mknod path, the environment through a re-exec of
  itself, and the cwd surface: `getcwd` in both forms, `strerror`'s text, and the
  `stat`/`readdir` dev/ino values a `..` walk compares). Each section prints one
  line only when every check in it held, and prints the failing `__LINE__`
  otherwise, but nothing in `tools/smoke/` runs it — the coverage exists only
  when a human types it at the shell. Verified by hand on 2026-09-24:
  `FEED_SCENARIO=...` with `/bin/ctest` and the lines `scanf: ok`, `mkfifo: ok`,
  `getenv: hello` passes, and the same run with an impossible readback fails, so
  the check itself is sound. A scenario step is what would stop it depending on
  that. (`tools/ctest.c`, `tools/build-c-hello.py`, `tools/smoke/feed.sh`)
- **bash is built by a scratch route and no gate boots it.** `/bin/bash` is not in
  `BOOT_BINS`, so it only reaches an image through `MINIXFS_EXTRA`, and the binary
  itself is the Linux-host build in `target/tmp/` (see `C_BUILD.md`) — nothing
  tracked rebuilds it. That makes two traps live whenever the libc changes:
  `make` reports success *without* relinking (the rlib is not a prerequisite of
  any bash target, so a `make exit: 0` with 203 objects and an unchanged binary
  mtime is the normal outcome — the mtime is the only evidence a relink
  happened), and `target/tmp/cc-minix` takes the *newest* `libminix_libc-*.rlib`
  in `deps/`, which after a Windows `just build-x86` is the other host's rlib
  (the link then fails `E0460: found possibly newer version of crate core`).
  Verified by hand: `/bin/bash -c 'printf "PWD=%s\n" "$PWD"'` prints
  `PWD=/tmp/cwdtest` from a scratch scenario, and before the forced relink the
  same step carried the `shell-init: error retrieving current directory` line.
  (`target/tmp/bash-*.sh`, `target/tmp/cc-minix`, `C_BUILD.md`)
- **~~`signal.h` and `minix-libc` disagree on `sigprocmask`~~ — FIXED
  (2026-09-24).** `minix-std`'s signature is now
  `sigprocmask(how, set_ptr, old_ptr)` (PM's `m2l1`/`m2l2`, both caller
  pointers), the libc export is the POSIX three-argument form, and
  `sigsuspend`/`sigpending` — which PM already implemented as
  `do_sigsuspend`/`do_sigpending` but nothing exposed — are now in `minix-std`,
  the libc and `signal.h`.
  (`tools/c-include/signal.h`, `crates/minix-libc/src/lib.rs`)
