# Known Issues — inventory across x86_64 / RISC-V / AArch64

Open issues are listed first, then the resolved ones under `# Resolved issues`; item numbers are stable across the move, so a `KNOWN_ISSUES N` reference still resolves. Detailed write-ups live in `.notes/` (e.g. `.notes/filemmap.md`). A platform or harness limitation captured in a skill sits in Resolved too, and says so.

Legend: `[all]` affects all arches, `[x86]` / `[riscv]` / `[aarch64]`
are arch-specific, `[env]` is tooling/platform, not kernel.

---

# Open issues

## Cross-arch (`[all]`)

5. **Kernel-mode fault attribution** — `handle_page_fault`
   attributes by `current_proc()`, which is wrong during a
   cross-address-space `virtual_copy` (CR3 owner vs. executor). The exec
   **pre-fault workaround stays** because of this gap, not a resume bug:
   with it disabled, RISC-V's S-mode forwarding fires but the kernel
   attributes the fault to the copier (e.g. VFS) and VM SIGSEGVs it.
   Removing the pre-fault requires fixing attribution first.

   **Decision (2026-08-14): avoid the wrong attribution rather than fix it,
   C-faithfully.** Pre-check page presence on **both** sides of every kernel copy
   and return `EFAULT` instead of fault-forwarding (C MINIX's `virtual_copy`
   pre-checks via `vm_check`). Implemented in `safecopy` (the granter's pages —
   `crates/kernel/src/grants.rs`) and `do_copy_common` (both src and dst —
   `crates/kernel/src/system.rs`), with `EFAULT`-on-missing-page host-tested in
   both directions. Consequences:
   - Under this model the exec **pre-touch/pre-fault is the design, not a
     workaround**, and stays; removing it is off the table until the deferred
     alternative below is taken.
   - The **deferred alternative** is fault-forwarding with (a) dual-endpoint
     attribution — VM resolves by the *owner* (fault-time CR3) and clears/blocks
     the *executor* — and (b) a kernel-mode resume that restores the **fault-time**
     CR3 (today `restore` loads the executor's `p_seg.p_cr3`). It is the riskiest
     kernel change in this area and should land only with a real consumer (lazy
     stack/heap, demand-paged kernel-copy targets).
   - The earlier fault-in machinery stays in place either way, dormant: the
     address-based gate in `handle_page_fault` (forward below `MAX_USER_ADDRESS`
     to VM, kernel-address faults stay fatal), the x86 `swapgs` +
     `save_fault_context` entry and kernel-mode `restore`, and the RISC-V S-mode
     forwarding.

   *Measured 2026-11-09: the exec pre-fault is where a large binary's per-invocation I/O goes, and it
   cannot be narrowed to writable segments.* `coreutils true`, which prints nothing and writes
   nothing, costs 1003 virtio reads of 4 KiB (3.9 MiB) over a boot's 49, because the first text
   fault pre-faults every non-executable file region of the 6 MB multicall (`Fault::for_prefault`).
   Restricting that to writable file regions leaves the read-only ones lazy, and the guest then
   never reaches a shell prompt: the shell's `write(1, "# ", 2)` has VFS copy a rodata literal out
   of its buffer in kernel mode, the pre-check finds the page absent, and the prompt write fails.
   So the pre-fault is load-bearing for every program that passes a string literal to a syscall,
   and dropping it needs either this item's attribution work or a *pre-copy touch*: the copier asks
   VM to make a user source range present before the kernel copies out of it, so VM fills the page
   through the owner's FDIO path instead of faulting in kernel mode. The touch is the smaller of
   the two, and both should be judged by the same trace (`tools/virtio_read_hist.py`).

   *Why the pre-copy touch is not small either (found while starting it, 2026-11-09).* A
   synchronous "make this range present" call from VFS cannot work: VM fills a file page by asking
   VFS (`vm/vfs_request.rs::send` → `VMVFSREQ_FDIO`, an `asynsend3`), so a VFS blocked on the touch
   would never answer the FDIO the touch is waiting for. The workable shape is the codebase's own
   async pattern: when a write-from-user copy returns `EFAULT`, VFS sends the touch, suspends the
   caller's write, keeps servicing FDIO in its main loop, and retries the copy when VM reports the
   range present. That is a new VM request with a completion, a suspended-write state in VFS, and
   `Fault::for_prefault` narrowed to writable regions. Its gate is `just test-read-cost-x86`:
   `coreutils seq 1` costs 1079 4 KiB reads today, so a lazy version has to land well below that.

   *Measured 2026-11-16: the pre-fault is a cold-start cost, which lowers the priority of the
   touch.* Two invocations of the same 6 MB binary in one boot cost 1088 4 KiB requests in total:
   the first, `coreutils seq 1`, is 1079 as above, and the second, `coreutils seq 2`, is 9,
   because VM's file cache already holds every page the pre-fault filled. A shell that runs many
   applets of one multicall therefore pays the pre-fault once per binary per boot, not once per
   command, so the async touch would cut that one-time 1079 to roughly 60 rather than cut a
   per-command cost. `just test-read-cost-x86` now types both commands and holds the total under
   1120 (1088 in every run so far), so one trace covers both the cold growth and the warm cache.

6. **Kernel-mode fault resume beyond exec** — all three arches' kernel-mode
   resume paths are in-commit and boot-verified, but the lazy coverage is
   exec-only (text fetch + pre-faulted regions). General kernel-mode copies
   into lazy user pages (future lazy stack/heap, general `vircopy`) still
   need the machinery exercised.

   Under the #5 decision, ordinary kernel copies pre-check and return `EFAULT`
   instead of faulting, so these resume paths are dormant rather than load-bearing.

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

7. **Server Phase 12/13 stubs** (not arch bugs, but known gaps). Only one gap is still open:
   **`procfs` is ported but not wired, and several of its handlers return fixed text**. The
   other bullets below describe work that has since landed and are kept as the record of what
   the port does now; `devman`'s main loop is one of them, and its old "is a TODO" wording is
   corrected here.

   `crates/fs/src/procfs` has the VTreeFS tree, the inode table and the buffer helpers, and
   `procfs_main` starts a real loop, but nothing calls it: there is no `procfs` bin, no
   `BOOT_BINS` entry, and VFS mounts no `/proc`. Its content handlers also return a fixed string
   where C's procfs asks PM, VM or the kernel (`root.rs` uptime, loadinfo, kinfo and vminfo;
   `cpuinfo.rs` machine and cpuinfo; `pid.rs` cmdline, environ and psinfo; `misc.rs` load
   averages). Wiring it is a boot slot, a VFS mount and those queries, and no consumer needs it
   yet, so this is recorded rather than started.
   - `tty` — `sys_safecopyto/from` (SYS_SAFECOPYTO/FROM kernel calls) and
     `chardriver_reply_task`/`chardriver_reply_select` (CDEV_REPLY /
     CDEV_SEL2_REPLY builders) are implemented and wired into `dev_ioctl`
     (TIOCDRAIN deferred reply) and `select_retry`. The deferred
     READ/WRITE/IOCTL protocol is implemented too: VFS hands the request
     over without waiting and the driver answers `CDEV_REPLY`, which
     `cdev_reply` -> `cdev_io_reply` completes (`70fbb3ff8`, gated per
     driver by `dmap_cdev_async`, set for the tty). So `cdev_reply` is no
     longer a stub for the `CDEV_REPLY` arm (the `SEL1`/`SEL2` arms still
     are), and ioctl args move through a magic grant sized by `ioc_size`,
     which the tty's `TIOCGETA`/`TIOCSETA*` codes carry as
     `size_of::<Termios>()`, so a full `Termios` round-trips. The last piece,
     the MIN/TIME read timer, is now implemented: `settimer` stores an absolute
     deadline (`tty_tmr_exp`) sized from `VTIME` at the kernel's tick rate, and
     `tty_expire_timers` fires it on the console pump's own wake, so no
     server-side timer queue is needed. The earlier note that servers had "no
     kernel timer API" was wrong: `SYS_SETALARM` exists, and the input server
     and VFS already use it. The timer can only complete a read because
     `handle_events` gained C's post-`in_transfer` reply rule
     (`read_ready_to_reply`), which the port had dropped: with `tty_min` forced
     to zero a held read answers at once with what it has, zero bytes included,
     and that rule is also what honours `VMIN` in raw mode. Gate:
     `just test-ttytimeout-<arch>` runs `/bin/ttytimeout`, a raw console read
     with `VMIN=0` and `VTIME=2` that must return zero bytes in its window.
   - `devman` main loop **landed**: `crates/servers/src/devman.rs` (1792 lines) serves the
     device tree through `vtreefs::start_vtreefs`, whose message loop
     (`crates/libs/src/vtreefs/mod.rs`) answers the port's FS protocol and RS's init request;
     `/sbin/devman` is a `BOOT_BINS` entry and VFS mounts it at `/devices`. `procfs` is the
     part of this entry that is still open, above.
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
     .notes/porting_plan.md finding 58.
     Still true, and unrelated to that: `vm_remap`, `vm_getphys` and
     `vm_unmap` in `servers/src/ipc.rs` are *callers* of VM, so a server
     blocking on VM in those paths is the ordinary shape, not a cycle.

29. **A driver cannot call VFS back while VFS waits on it (2026-09-27, settled for sockets,
    open for VND).** MINIX's drivers make privileged *back-calls* into VFS during an ioctl: the
    uds driver moves passed descriptors with `copyfd` (`ioc_uds.c` `send_fds`/`recv_fds`), and
    VND opens its backing file with `copyfd` plus `fstat` (`vnd.c`). MINIX answers those on a
    second VFS thread while the calling thread waits for the driver's ioctl reply. This port's
    VFS is single-threaded and parks in `request::fs_sendrec` (`minix_std::sendrec`), so a
    back-call would sit behind a receive that never comes — a deadlock, not a slow path.

    **Sockets are settled by moving the work to VFS, not by making VFS reentrant.** VFS
    intercepts the two `NWIOSUDSCTRL`/`NWIOGUDSCTRL` ioctls and does the descriptor transfer
    itself (`crates/servers/src/vfs/scm.rs`); the driver's only new duty is answering "which
    socket is your peer" in its ioctl reply, and `NWIOGUDSPEERCRED` is served from the peer's
    recorded owner endpoint. `do_copyfd` stays implemented (`VFS_COPYFD`, root-only) for a
    caller that genuinely has a thread to be answered on, but nothing in the tree uses it yet.
    See `.notes/wayland.md` §5 row 5 and §6.2.

    **VND is still open.** `vnd_copyfd`/`vnd_fstat` (`crates/drivers/src/storage/vnd.rs`) return
    `Err(DriverError::Unsupported)`, so `VNDIOCSET` cannot open a backing file. Either give VFS
    the same treatment (it would have to learn VND's ioctls, as it has learned the socket ones),
    or introduce a mechanism to service a driver's back-call — which needs VFS's per-request
    state saved around the nested call. No consumer needs VND yet, so this is recorded rather
    than resolved. (`crates/servers/src/vfs/scm.rs`, `crates/servers/src/vfs/call.rs::do_copyfd`,
    `crates/drivers/src/storage/vnd.rs`)

38. **An `exec` of a command can fail at random (2026-11-16, open, measured).** The
    interactive-bash gate failed in CI on aarch64 and on riscv64 (`bash never printed its
    prompt`), the console showing the prompt's reprint of the line and then a message for a
    `/bin/bash` that is in the image: `mkminixfs` reported it as part of `100 files` with no
    missing-bash warning, and `mkfs` copies that image straight to the disk the guest boots. So
    the file was there and the `exec` returned. Reproduced once locally in 3 runs, then not in 26
    consecutive runs since. Suspects, in order: `minix-rt::execve`'s own `sbrk` of the argument
    frame, which can return `ENOMEM` before the syscall is made; VM's loader on the 1.6 MB image;
    and the allocation-heavy-path class aarch64 item 9 describes. The `execve` offset-table
    overflow in item 45 is one candidate for this class and is fixed.

    *Both layers that hid the errno are now fixed, so the next occurrence reports the real one.*
    The shell printed `not found` for every failure of `try_exec` (`report_exec_failure`/
    `format_errno` in `crates/userland/src/shell.rs` now print the path and the kernel's number),
    and `minix_rt::execve` returned the SENDREC's *return value* rather than PM's reply, so even
    that number was the replier's endpoint, positive, reported as `-1`. It reads the reply's
    `m_type` now (`exec_reply_status`, host-tested beside `chdir`/`fork`/`brk`), and the `/sbin`
    fallback is tried only when `/bin/<cmd>` was `ENOENT`, so a substantive error is no longer
    replaced by the fallback's own `-2` (`should_try_sbin`, host-tested). Next step: read the
    errno from the next failing gate, in CI or locally. It has not failed in 26 runs since the
    first half of the diagnostic landed, so the errno is still owed.

---

## RISC-V (`[riscv]`)

No open issues. The RISC-V clock, heap, trap-state and FP-image defects in item 6, and
the burst-overflow reading in item 37, are under `# Resolved issues`.

---

## AArch64 (`[aarch64]`)

49. **`/bin/threadtest` takes an unhandled EL0 exception on aarch64 once a worker runs
    (2026-11-17, open, reproduced).** `just test-threads aarch64` runs `tools/smoke/threads.tsv`
    against `/bin/threadtest`, which spawns four kernel threads that block in PM IPC, bump a
    shared counter and are joined. The same binary and the same scenario pass on x86_64 and
    riscv64. On aarch64 the command prints at most one byte of its `threadtest: spawned` line and
    the shell is back at its prompt: no error from the program itself, so it was killed.

    Two faults were in the way of seeing this, and both are now fixed (this commit). The
    unknown-exception report in `arch-aarch64/src/exception.rs` wrote a bare `X` to the UART's
    **physical** address `0x0900_0000`. With the MMU on that address is not mapped (devices are
    reached through the physmap), so the report itself took a kernel-mode store fault, which
    `handle_page_fault` forwards to VM as a user address, and VM answered `sys_kill(SIGSEGV)` to
    whichever process was running: the trace showed the same kill at `0x0900_0000` repeating
    forever, which read as a wild user store and was not one. The report now goes through
    `serial_write_byte` and prints the class, ELR, SPSR, ESR and FAR.

    With that fixed the real event is visible: `EL0 unhandled exception, EC=0x00`, `ESR=0x02000000`
    (ISS zero), `ELR=FAR=0x0000_0000_0100_3060`, `SPSR=0x8000_0000` (EL0t). EC 0 is ARMv8's
    "unknown reason", the class an **undefined instruction** reports, and `0x1003060` is inside
    the program's own `.text` (the `memset`/`memcpy` functions). The instruction that belongs at
    that address in the linked binary is a plain `add x8, x8, x10`, so the fetch is not seeing
    the file's instruction: a zero-filled page (`0x00000000` is `udf #0`) is the likely content,
    and the same address had earlier been seen taking a *translation* fault, which fits a page
    that is mapped without being filled from the file.

    This is also why a diagnostic `write` in the main thread makes the run pass: the write's own
    formatting code runs on that `.text` page, so the main thread faults it in first, from the
    file, before any worker fetches it. The open question is therefore narrow: why a file-backed
    `.text` fault taken by a **worker** leaves the page unfilled (or zero-filled) when the same
    fault taken by the main thread fills it from the file. The next step is to have the worker
    read the instruction word at `0x1003060` with a volatile load and print it before it calls
    into that page, and to trace `handle_pagefault_for`'s region flags for the address (VR_FILE
    set or not) on that path.

    Also unchanged, and separate: an unhandled EL0 class reports and **halts** the machine, which
    is the pre-existing policy for a class the dispatcher does not know. Delivering SIGILL to the
    process would be the user-visible behaviour to aim for, and it needs a kernel-side path from
    the vector, not a report.

The two standing notes (probe/stub traps, and the dual page-table
layout) are captured in the `aarch64-debugging` skill and sit under `# Resolved issues`,
as this file's header says a skill-captured limitation does.

---

# Resolved issues

## Cross-arch (`[all]`)

8. **~~Threads (THREADS.md open workstreams)~~: the API surface landed; one defect remains
   (2026-11-17).** The entry was stale. `thread_local!` works: the minix std PAL allocates a
   TLS block and installs the thread pointer at start-up and for every spawned thread
   (`rust/library/std/src/sys/pal/minix/mod.rs`, `minix_rt::thread_set_tls`).
   `std::sync::{Mutex, Condvar, RwLock, Once}` are the generic futex implementations over
   `minix_rt::futex_wait`/`futex_wake` (`rust/library/std/src/sys/sync/futex/minix.rs`), and
   `std::thread::spawn` creates a kernel thread (`rust/library/std/src/sys/thread/minix.rs`
   over `SYS_thread_create`). The C-ABI pthread surface is `crates/minix-libc/src/pthread.rs`
   (THREADS.md Slice 3) and `errno` is a thread-local in its TLS block. The kernel model
   (1:1, fork/exec/exit group sweep, and thread-aware IPC receiver matching in
   `kernel::ipc::find_receiver`) was already boot-verified.

   What was really missing was a gate. `/bin/threadtest` and `/bin/forkthread` were both in
   `BOOT_BINS` and built into every image, and no recipe ran them. `just test-threads <arch>`
   now drives both through `tools/smoke/threads.tsv`: four kernel threads that block in PM
   IPC, bump a shared counter and are joined, then a fork from a worker whose child must be a
   copy of that worker rather than of the main thread.

   `test-threads x86` and `test-threads riscv64` are in `test-arches` and pass. `aarch64`
   fails, now open as item 49, so its recipe stays a command a person runs until that is
   fixed.

   Still deferred and unchanged: per-thread sigreturn. Signals are delivered per process, so
   a signal lands on the main thread's frame, which is what every consumer in the tree uses.

35. **~~`epoll` ignores `EPOLLONESHOT`~~: FIXED (2026-11-17).** An `EPOLLONESHOT`
    interest is now disarmed by its own report, as on Linux: it stays in the set
    (`EPOLL_CTL_MOD` re-arms it, `EPOLL_CTL_DEL` removes it) but no wait scans it until
    then. `Interest` carries an `armed` flag that `snapshot` skips, `EPOLL_CTL_ADD` and
    `MOD` arm it, and `write_epoll_events` calls `epoll::after_report` for each event it
    actually writes back, so a report the `maxevents` cap pushed out of the call does not
    disarm. Without the rule a consumer that takes one event and deliberately does not
    drain is woken again by every level-triggered re-scan, forever.

    The rest of the entry is a deliberate limit, not a defect, and stays recorded in
    `.notes/wayland.md` §5 row 23: `EPOLLET` is accepted and served level-triggered (the
    engine keeps no ready list, so a true edge cannot be told from a repeat; a consumer that
    drains to `EAGAIN` sees no difference beyond the extra reports); an instance holds at
    most `MAX_INTERESTS` (64) registrations, answering `ENOSPC` beyond that; a registration
    whose fd the waiter has closed is pruned at the next `epoll_wait` rather than at `close`;
    `EPOLLERR`/`EPOLLHUP`/`EPOLLRDHUP` are not synthesised, matching `poll`; an epoll fd in a
    *`poll`/`select`* watch reads as always-ready; and an instance's interests resolve in the
    *waiter's* fd table. `calloop` (and so smithay) uses level-triggered `EPOLLIN`,
    deregisters before closing, and registers and waits in one process, so none of these
    bites it.

    Gates: `just test-epoll-x86` now requires the oneshot case (report once, a `wait` with
    the counter still nonzero returns 0, `MOD` re-arms and it reports again) and fails
    without the fix ("a disarmed oneshot interest reported again"); two host tests pin
    `snapshot`/`after_report` for a oneshot and a level interest.
    (`crates/servers/src/vfs/epoll.rs`, `crates/servers/src/vfs/select.rs`,
    `crates/userland/src/lib.rs`, `crates/minix-std/src/fs.rs`)

1. **~~VM block cache is a stub~~ — FIXED (2026-11-01).** The entry was stale: `do_mapcache` maps the
   cached blocks into the caller, `do_setcache` registers caller-owned pages with their own
   reference, `do_clearcache` drops a device's entries, and `crates/servers/src/vm/cache.rs` is a
   `CACHE_CAPACITY` (4096) LRU table the file-fault path (`start_file_page`) consults before asking
   VFS for a page. What is still true is that `read(2)` does not go through it, because that is the
   filesystem's own cache (item 2).

2. **MFS read-path I/O amplification (fixed in `8e5e397c4`, 2026-08-14; measured again at
   about 1 I/O per zone on 2026-11-09).** Large sequential reads were
   recorded at 3-6x the file's block count. The cause was `DEV_RAM = 0` colliding with the
   root filesystem's device number 0: `lmfs_put_block` sent every block of device 0 to the
   LRU front, so the shared indirect block was evicted between data chunks and re-read for
   each one (`8e5e397c4`, pinned by
   `libminixfs::cache::test_module::test_sequential_read_io_count_stays_near_block_count`). Measured again
   on the x86 guest with QEMU's `virtio_blk_handle_read` trace: every request is 8 sectors
   (4096 bytes, one MFS zone), and reading a 6,189,776-byte file costs 1527 requests beyond
   the reader's own I/O (1511 zones and 16 for metadata), so the read path is about 1.01x.
   Reading the same file through `coreutils wc` costs 2590 requests in total; the same command
   with an option it rejects (the applet starts, prints usage and reads no file data) costs
   1063, and the 1527 difference is the file read. That 1063 is `coreutils`' own exec and
   startup: its 6 MB image is read for the non-executable regions the exec pre-faults eagerly
   (`filemmap.md` §5), which is a per-command cost item 5 owns, not MFS's.
   The two candidate fixes recorded before (read on `PREFETCH`, move `ONE_SHOT` off
   `FULL_DATA_BLOCK`) were reverted correctly: neither was the defect. One measurement note:
   the trace backend perturbs the stdio console badly enough that a traced run must verify
   its command's own output before the counts are believed. (`.notes/filemmap.md` §7)

3. **~~`mmap(fd)` MAP_SHARED semantics~~: FIXED (2026-11-17).** File-backed
   `MAP_SHARED` has set `VR_SHARED` and shared the file-cache frames since the cache
   landed, and `mmapfd shared` checks the child's write in the parent's view. The
   reverse case, a shared *anonymous* mapping, is now implemented too: `do_mmap`
   allocates a `MAP_SHARED|MAP_ANONYMOUS` region's frames at map time (a lazy page
   faulted after the fork would get its own frame), marks it `VR_SHARED|VR_PRESENT`,
   and leaves the existing `VR_SHARED` COW exception to re-enable writability on the
   shared frame. `just test-mmapanon-x86` runs `mmapfd anon-shared`, which touches
   nothing before the fork and requires the child's first store to be visible in the
   parent; it fails on the old lazy path (`parent did not see anon-shared write`).
   The remaining limitation is the file page cache: an evicted `MAP_SHARED` page is
   dropped from the cache while a mapping keeps its frame, and a later mapping of the
   same file page reads the file again, so the shared write is not visible there and
   nothing is written back. That is recorded as item 47.
   (`crates/servers/src/vm/mod.rs`, `crates/userland/src/lib.rs`,
   `tools/smoke/mmapanon.tsv`, `Justfile`)

32. **~~A `read` never blocked in VFS~~: FIXED (2026-11-17).** A pipe read of an empty pipe
    with a writer open, a write that did not fit the buffer, an `eventfd` read on a zero
    counter and a `timerfd` read before its expiry all answered `EAGAIN` instead of waiting,
    because VFS had no way to suspend a request that could not proceed. A pipeline whose
    consumer ran before its producer therefore failed at random.

    The mechanism is C's, without C's worker threads, because this port's VFS is one thread:
    a handler that cannot proceed parks the request in `crates/servers/src/vfs/blocked.rs` and
    returns `SUSPEND`, which the main loop already reads as "do not reply"; whatever makes
    progress resumes the request and sends the answer through `misc::reply_endpoint`. The
    resumers are `pipe::release` (called after a transfer moves the other direction and from
    `close_filp` when an end goes away), `eventfd::resume` (called by `write`) and
    `timerfd::resume` (called by `expire`, so the alarm tick answers a waiting read).
    `free_proc` drops a parked request before it closes the dying process's fds, and
    `EPOLL`/`EAGAIN`-style callers are unaffected: `O_NONBLOCK` still answers at once, which
    is what `select`, `poll` and `epoll` are built on.

    Two defects surfaced while making it work. `close_filp` released the *wrong* direction
    (closing a write end resumed writers rather than readers), which deadlocked the first
    `sleep | cat`; and VFS believed a pipe held `PIPE_BUF_SIZE` = 7168 bytes while
    `crates/fs/src/pfs/consts.rs` gives the buffer 4096, so any write between the two numbers
    failed with `EFBIG` the moment a blocked writer was resumed with the rest of a large
    buffer. They are now the same number, named once in each place.

    Gates: `just test-pipeblock <arch>` runs `/bin/pipetest`, which forks with the other side
    standing still for 200 ms and requires a read of an empty pipe to receive a late write and
    a 20 KB write to push through a 4 KB buffer (it fails without the fix: "a blocking read did
    not wait for its writer"); `eventfdtest` and `timerfdtest` gained the same read cases, and
    three new host tests pin the release direction, the park/discard bookkeeping and the
    readiness rule that treats `SUSPEND` as "not ready".

    Still deliberate, and not reads: a *blocking open* of a named FIFO with no peer answers
    `EAGAIN` (`pipe::pipe_open`), and an `eventfd` write that would overflow the counter
    answers `EAGAIN`; neither is reachable from this port's consumers.

13. **~~Free slots are left not-quite-free~~ — FIXED (2026-11-01).** `Proc::is_empty` tests the
    `SLOT_FREE` bit, the way C's `get_free_proc` does (`RTS_ISSET`), instead of equality with it, so
    a slot an exit left holding `SLOT_FREE|P_STOP` or `SLOT_FREE|NO_ENDPOINT` reads as free. Every
    caller of `is_empty` follows it with an endpoint check, so the bit is the answer they wanted.

15. **~~`write_to_proc`/`read_from_proc` silently ignore their `proc` argument~~ — FIXED
    (2026-11-01).** Both switch CR3 to the target for the copy and restore the caller's
    (2026-09-22), and the part that stayed open is fixed too: `vm_memset`'s only bare `write_bytes`
    is the `KERNEL_ADDRESS_SPACE` arm, where the address really is the kernel's own, and every
    process target goes through `write_to_proc` in 256-byte chunks.

17. **~~The kernel's `brk` handler keeps one global `CURRENT_BRK`~~ — FIXED (2026-11-01).** The
    break is per process (`Proc::p_brk`): `proc_init` starts every slot at the HAL's heap base,
    `fork` inherits it with the rest of the `Proc`, `exec` resets it to the base, and the handler
    reads and writes the caller's own field. Removing the handler was not an option: it is
    reachable, `crates/wasm-procs` calls syscall 36 and the host grows the instance's memory in
    response. New test `test_breaks_are_per_process`.

18. **~~Two physical page allocators are initialised over the same RAM~~ — FIXED (2026-11-01).**
    The x86 boot path no longer hands `kernel::vm::mem_init` the whole `[kernel_end, mem_top)`
    range (and the stale comment claiming kernel call 62 used it is gone;
    `do_vm_paging_handler` allocates from `hal::alloc_phys_contig`). The only remaining user, the
    in-kernel test suite, reserves its pool through the arch allocator first
    (`test_runner::init_vm_allocator`, and the same pattern in `kernel/src/tests.rs`), so the two
    allocators are disjoint on every arch by construction. The test that pinned a fixed pool
    address (`alloc_lower16mb`) now asserts that `PAF_LOWER16MB` follows wherever the pool ended up.

26. **~~`isatty` answers 1 for any of fd 0..2~~ — FIXED (2026-11-01).** `crates/minix-libc`'s
    `isatty` asks the descriptor instead of its number: `fstat` and `S_IFCHR`, with `ENOTTY` for a
    regular file, a pipe, or a descriptor that cannot be `fstat`ed. bash's interactive decision and
    coreutils' piping decisions see the truth now, so a redirected shell is no longer told it is on
    a terminal.

41. **~~`VM_GETPHYS` puts the address and the reply in different fields on each side~~ — FIXED
    (2026-11-01).** One layout for all three sides (`servers/src/vm/mod.rs`'s `do_get_phys`,
    `servers/src/ipc.rs::vm_getphys`, `servers/src/fb.rs`'s scanout path): target at bytes 8..12,
    address at 16..24, and the physical address answered as a `u64` at 8..16, the convention
    `do_mmap` and `do_map_phys` use. The reply is no longer truncated to 32 bits, and an unmapped
    page answers 0, which is also what a failed call answers (frame 0 is never a user page).

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

9. **Fork-from-worker-thread ambiguity — DONE (2026-08-18).** The forking
   tid now flows through the whole fork protocol (userland `fork()` →
   PM_FORK → VM_FORK → SYS_FORK; new `SYS_thread_self` = 64 returns the
   caller's `p_tid`), so the kernel's `do_fork_handler` copies the exact
   forking thread's frame via `find_thread_by_tid` instead of the
   best-effort scan (which stays only as a fallback). Verified by
   `/bin/forkthread` (fork-from-worker, child inherits the worker's tid)
   on x86.

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
    `test_minix_ipc_error_codes_match_c` pinning them. (`.notes/porting_plan.md`
    finding 20)

10. **~~Userland `mmap(fd)` has no real consumer~~: resolved (2026-11-17), the entry was
    stale.** `wlserver` is a production `mmap(fd)` user and has been since the `wl_shm`
    present path landed: `compose_scene` maps each client's pool memfd by the fd it
    received over the socket (`crates/servers/src/wlserver.rs`), `load_keymap` maps the
    keymap memfd it builds, and the present path maps the fb and dma-buf fds;
    `wserver` maps `/dev/fb` the same way. `mmapfd` is therefore a focused test, not the
    only user. (`.notes/filemmap.md` §6)

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

14. **A blocking console read parks VFS — FIXED (2026-10-09; filed 2026-09-21).** VFS's main loop is a
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
    `cdev_reply`).

    **FIXED 2026-10-09.** That deferred path landed (`70fbb3ff8`): VFS hands READ/WRITE/IOCTL
    to a driver without waiting and suspends the caller (`cdev_io`/`cdev_insert`; the driver
    answers `CDEV_REPLY`, which `cdev_reply` -> `cdev_io_reply` completes), so a console read
    no longer holds VFS. Gated per driver by `dmap_cdev_async` (set for the tty). The wasm
    target has no timer to drive the console pump, so its `do_read` keeps a ring poll; the
    hardware arches suspend.

16. **A new process's root mapped low physical memory with *user* access -- FIXED (2026-10-02).**
    `exec_elf_for_target` builds the fresh root with `exec_create_root`, whose own comment said it
    copies "the identity map with **user access** in the low window". Measured live on aarch64 with
    `/bin/winprobe` (`just test-winprobe <arch>`): a shell-exec'd process could read *and write* the
    kernel's physical allocator through VAs it was never given. `create_low_gb_pmd_table`
    (`arch-aarch64/src/hal.rs`) filled PUD[0] with 2 MB blocks carrying `PMD_BLOCK = 0x741`
    (`AP[2:1] = 01` = `PTE_AP_EL0_RW`) whose physical address was
    `win_base + ((va - user_low) % win_size)`, so the whole low GB aliased free RAM. The branches it
    called "unmapped" (below `user_low`, and `win_size == 0`) wrote `0 | PMD_BLOCK` -- a *valid*
    EL0-RW block at physical 0, not an invalid entry. riscv64 and x86 were unaffected (`read=err`):
    riscv64's `exec_create_root` does not copy the window's entry at all (`.notes/physmap.md` P4) and x86's
    identity map stops at the user base.

    *Fixed (2026-10-02).* The table now maps only the virtio-mmio window (identity, EL0_RW) and
    leaves every other low-GB VA invalid -- the shape `boot_create_restricted_page_table` already
    built for the boot processes, so an exec'd process now matches instead of receiving RAM at EL0.
    `map_page` allocates a level-3 table when a real mapping lands in an invalid block, so the image,
    heap, stack and mmap still install on demand. Measured after: `just test-winprobe aarch64`
    reports `probed=4 reachable=0` on all four VAs (it was red on purpose before), and
    `just test-arches` (boot to shell, dynlink, fork) stays green. `test-winprobe` is promoted into
    `test-arches`.

    The boot test's M1 check could not see it: loop 1 *required* a present entry in the user window
    to carry `PG_U`, and loop 2 walks kernel VAs only, so a user VA aliasing kernel physical memory
    satisfied both. Loop 1 is now tightened for the fresh exec root: every present entry in the user
    window must be the device window (`test_user_and_kernel_mappings_do_not_overlap`).

    The alias *recognition* machinery -- `is_alias_frame`'s RAM branch, `alias_window`/
    `set_alias_window`, and VM's `VM_PAGING_MEMINFO` -- is now unreachable in practice (no mapping
    classifies as an alias), and can be removed in a follow-up; only the `frame == va` identity case
    is still needed, for the device window's split leaves.

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

24. **~~`std::env::current_dir` has no implementation for minix~~: FIXED (2026-11-17).**
    The fork's `sys/paths/mod.rs` routes `target_os = "minix"` to
    `sys/pal/minix/path.rs`, whose `getcwd` walks the cwd up to the root and back down
    through `minix_std::fs::getcwd` (ported from MINIX's `__getcwd`), growing the buffer on
    `ERANGE`; the same module implements `chdir` and `temp_dir`. The fix was already in the
    fork (`d2280b1c34e`, "std: add getcwd, dirent inodes and exec to minix"); the entry was
    stale. Verified in the guest with the current stage1: `coreutils pwd` prints `/`, a new step in
    `tools/smoke/coreutils-wedge.tsv` driven by `just test-coreutils-wedge` (6 steps green).
    (`rust/library/std/src/sys/paths/mod.rs`,
    `rust/library/std/src/sys/pal/minix/path.rs`, `crates/minix-std/src/fs.rs`,
    `tools/smoke/coreutils-wedge.tsv`)

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

27. **A fork shared the parent's writable pages with the child, so the parent's own writes
    reached the child (2026-09-24, FIXED).** `vm_paging_fork` clears `PG_RW` on the child's
    writable user leaves and leaves the parent's alone, so after a fork the parent still had
    write access to the very frames the child aliased. Every write the parent made between the
    fork and the child's first store to a page was therefore visible to the child — the fork was
    not the snapshot `fork(2)` promises, in the direction nobody tested (`/bin/forktest` only
    checked that the *child's* write did not reach the parent).

    *Found through bash (see `.notes/c_build.md`).* A command that forks — `cat /etc/passwd`, and any
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

    *Superseded (2026-11-01).* The eager per-page copy was the fix while the kernel had no way to
    write through a read-only user page; it also made a fork cost its whole address space (a
    compositor spawning a client copied 248 MiB, and could not do it at all in 512 MiB). The fork
    is lazy now: both sides' writable leaves are cleared, `cow_setup_fork` only takes a reference
    per shared frame, and the kernel breaks the copy itself before a grant, a message delivery or
    a signal frame (`cow_prepare_write`, with the abandoned frame handed back to VM).
    `/bin/forktest`'s third phase is the `vm_suspend`-less answer to the trade above: the child
    never writes its `SINK` page, the kernel fills it with a file read, and the parent must still
    see its fork-time page. `.notes/wayland.md` 4f has the whole shape.

28. **~~Cloned character devices were not visible through `fstat`~~: FIXED (2026-11-17).**
    MINIX gives every clone open its own temporary device node whose `i_dev` is the clone
    (`cdev_clone`), so `fstat` names the clone in `st_dev` and `st_rdev`. This port has no
    clone nodes: `do_open` records the clone device on the filp (`filp_dev`), and `do_fstat`
    forwarded to the filesystem unchanged, so a socket's `fstat` reported the filesystem's
    device in `st_dev` and the base device in `st_rdev`. `do_fstat` now composes C's answer
    for exactly the clone case (`is_cloned_char`: a character device whose open recorded a
    device different from the vnode's): the stat is requested into VFS's own buffer,
    `st_dev` and `st_rdev` are set to the clone, and it is copied to the caller. Everything
    else keeps the filesystem's answer. `st_ino` remains the opened node's, because there is
    no second inode here, and nothing that asks for a clone reads it. The rest of the
    entry's decision still stands: no temporary nodes and no `v_sdev`, and `NWIOGUDSMINOR`
    stays the port's own way to ask a socket for its minor. Gate: `just test-uds-x86`;
    `/bin/udstest` now requires `st_dev == st_rdev` with a real major and a non-zero clone
    minor, and a build with the branch disabled fails it with `fstat does not name the
    cloned device`. Host-pinned by `test_only_a_cloned_char_device_reports_the_open_device`.
    (`crates/servers/src/vfs/call.rs`, `crates/userland/src/bin/udstest.rs`,
    `crates/minix-std/src/fs.rs`)

30. **~~A `memfd` answered only some of the operations a filesystem would~~: FIXED (2026-11-17).**
    A memfd has no filesystem (its `v_fs_e` is NONE and `v_fs_count` is 0, which is what lets
    `put_vnode` skip `req_putnode`), so every call VFS did not answer itself travelled to an
    endpoint of NONE and failed: `futimens` on a memfd, `fsync` on a memfd, and `fstatvfs`, which
    has no storage to measure. `vfs::memfd` now answers all three. `do_utimens` gained the
    fd form C's `time.c` already has (it rejected `name == NULL` outright), so `futimens` resolves
    the descriptor's vnode and, for a memfd, stores the times in the vnode (new `v_atime`/`v_mtime`,
    since an inode is what holds a filesystem file's times) and `fstat` reports them. `fsync`
    succeeds: there is nothing to flush. `fstatvfs` describes the *object* rather than inventing a
    filesystem for it: page-sized blocks, `f_blocks` the object's pages, one file, nothing free,
    `f_fsid` the object's identity, `f_namemax` zero. Nothing on the `wl_shm` path
    (`memfd_create`, `ftruncate`, `mmap`, `SCM_RIGHTS`, `read`/`write`) reaches any of them, which
    is why they were recorded rather than synthesized before. Gate: `just test-memfd-x86` (now in
    `test-arches`); `/bin/memfdtest` checks each call and what `fstat` reports afterwards, and a
    build with the memfd branch in `do_utimens` disabled fails it. Pinned on the host by
    `test_statvfs_describes_the_object_not_a_filesystem` and
    `test_set_times_keeps_seconds_on_the_vnode`. Still absent, and now a separate question: there
    is no C `futimens` declaration in `tools/c-include` or `minix-libc`, so C code can only reach
    the fd form through `minix_std::fs::futimens`.
    (`crates/servers/src/vfs/{memfd,call,types,anon,glo}.rs`,
    `crates/minix-std/src/fs.rs`, `crates/userland/src/bin/memfdtest.rs`,
    `.notes/wayland.md` §5 row 18)

31. **A blocking `select`/`poll` on a pipe never wakes without a timeout (2026-09-28, FIXED).**
    The readiness engine (`crates/servers/src/vfs/select.rs`) asks `pipe_check` whether a pipe fd
    is ready, and a character device registers a late watch (`CDEV_SELECT`), but a pipe registered
    nothing: a blocking `select`/`poll` on a pipe with no timeout sat until its deadline — and
    with none, forever — even after the other end wrote. Nothing on the Wayland path polled a
    bare pipe, so it was recorded rather than fixed; closing it removed the last wake in the
    engine that could be lost.
    A pipe has no driver to report readiness to, because VFS *is* the device (the data is in PFS
    and the size is cached on the vnode), so there is nothing to notify and nothing to re-ask over
    IPC: the read, write or close that changed the pipe calls `vfs::select::wake_pipes`, which
    re-evaluates the FIFO descriptors of every suspended wait and completes the ones now
    satisfiable. Only FIFOs, deliberately — a pipe read must not drag the sockets in the same wait
    through another `CDEV_SELECT`. Closing is hooked too: a reader sees EOF, and a writer `EPIPE`,
    only when the *other* end goes away, and nothing else reports that.
    The reference instead records the watcher on the filp (`filp_pipe_select_ops`/
    `filp_pipe_select_ep`, `select.c:501-502`) and walks the filp table calling `select_callback`
    when a read or write completes (`pipe.c:386-401`). Those two fields stay unused here: they
    carry specific ready ops to *one* watcher, and this port's waits are level-triggered and
    re-evaluated, which also lets more than one wait watch the same pipe.
    Gate: `just test-select-x86` (`/bin/seltest`), which ends with a poll on a pipe that carries
    no deadline: a forked child writes, and the poll must come back with the byte.
    (`crates/servers/src/vfs/select.rs`, `crates/servers/src/vfs/pipe.rs`,
    `crates/servers/src/vfs/filedes.rs`, `crates/userland/src/lib.rs`)

33. **~~`CLOCK_MONOTONIC` is time since boot, *not* `boottime + uptime`~~: accepted deviation,
    recorded in `.notes` (2026-09-28; resolved as an inventory entry 2026-11-17).** MINIX
    3.3.0's `do_gettime` adds `boottime` to **both** clocks (`servers/pm/time.c`:
    `sec = boottime + clock / system_hz`), so its `CLOCK_MONOTONIC` is offset by the epoch
    seconds at boot, and moves if the wall clock is reset, which a monotonic clock must not
    do. This port returns the uptime tick count alone for `CLOCK_MONOTONIC`
    (`crates/servers/src/pm.rs`, `clock_gettime_reply`), matching POSIX and the port's own
    `clock_server` (`clock_time_to_ts`); `CLOCK_REALTIME` keeps the `boottime` offset. The
    concrete reason is an absolute `CLOCK_MONOTONIC` deadline (`TFD_TIMER_ABSTIME`, exactly
    how `calloop`'s timer arms): it is named in the same units
    `clock_gettime(CLOCK_MONOTONIC)` returns, and that has to be the kernel tick timeline
    `vfs::alarm` (`SYS_SETALARM`) arms against. With MINIX's offset the deadline landed
    `boottime` seconds away (~1.79e9 ticks on the x86 image). The deviation, its rationale
    and the consequence of undoing it are in `.notes/wayland.md` §5 row 22 and §9; this is
    not a defect waiting for a fix, so it no longer belongs in the open list.
    (`crates/servers/src/pm.rs`, `crates/servers/src/alarm.rs`,
    `crates/servers/src/vfs/timerfd.rs`)

34. **A kernel timer must leave its queue before it is re-armed (2026-09-28, FIXED).**
    `tmrs_settimer` re-inserts a timer, and the reference *removes it first*
    (`.refs/minix-3.3.0/minix/lib/libtimers/tmrs_set.c`: `tmrs_clrtimer(tmrs, tp, NULL)`
    before the insert). The port skipped that, so a timer re-armed while already queued was
    spliced into the list twice and corrupted it. It surfaced as deadlines that never fired:
    `SYS_SETALARM` re-arms the *same* per-process timer every time `vfs::alarm`'s earliest
    deadline changes, so `arm_earliest` re-arms a live timer on each change.
    (`crates/kernel/src/clock.rs`)

36. **A UDS socket could not wake `poll`/`epoll` (2026-09-28, FIXED).** Three faults, found
    together when `/bin/waylandtest` moved from retry loops to a real `poll`: (a) VFS's
    `select_request_char` asked the driver about `vnode.v_dev` — the **base** minor — while a
    socket's minor is the *clone* the driver returned at open and recorded in `filp_dev`
    (read/write/ioctl already used `filp_dev`; only select did not), so `CDEV_SELECT` named the
    wrong socket. (b) The `uds` driver answered `CDEV_SELECT => 0` and held no watch, so
    nothing could wake a waiter; it now answers the ready ops and holds one late watch per
    socket, woken on data queued, a connection queued or accepted, or a peer closing. (c) A
    report *pushed* from the driver cannot be relied on either, and the first fix for it failed
    in a way worth recording: `SENDNB` only lands while VFS is blocked in `RECEIVE`, and a driver
    retrying on a short `SYS_SETALARM` tick phase-locks with VFS's *own* deadline alarm (both 2
    ticks, armed at the same moment), so VFS was awake at every retry and answered `ENOTREADY`
    for the whole run — measured, with the notification queue stuck at two entries and
    `SYS_SETALARM` returning `OK` throughout. The driver now ends a readiness change with a
    `NOTIFY` instead — a *notification*, which the kernel remembers for a destination that is
    not receiving and hands over on its next `RECEIVE`, exactly as the input server already does
    for its consumers — and VFS re-asks its suspended waits on that notification
    (`vfs::select::rescan_suspended`). Readiness is level-triggered, so a lost push costs
    nothing, and the socket path now holds no reply queue and raises no alarm of its own. Gates:
    `just test-wayland-x86` (a forked client and server over `/dev/uds`, blocking on `poll`),
    `just test-wlshm-x86` and `just test-wlkey-x86` (`/bin/wlclient` and `/bin/wlkey` against
    `/sbin/wlserver`), `uds`'s host tests for the readiness state machine, and the Phase 0
    readiness gates. (`crates/servers/src/vfs/select.rs`, `crates/servers/src/uds.rs`,
    `crates/servers/src/vfs/main.rs`, `crates/userland/src/lib.rs`)

    The same trap on the terminal path is fixed with it: `tty.rs`'s line `select_retry` used to
    *push* its report with `SENDNB` (C's `chardriver_reply_select`); it now notifies
    (`tty_notify_select`) and lets VFS re-ask, so a `select`/`poll` on a tty line cannot lose a
    wake either. VFS still consumes a `CDEV_SEL1_REPLY`/`CDEV_SEL2_REPLY` as the reference ABI,
    but no driver in the port sends one. The pty master's half is wired as well, and it shows the
    same lesson in the hook's shape: `PtyHost::reply_select(minor, ops)` became
    `select_wake(proc)`, because what a driver can honestly report is *that* readiness changed —
    not what changed, which VFS asks for itself. The pty wakes from `slave_write`/`slave_echo`,
    and the TTY server from `pty_slave_close`, since a closed slave makes its master readable
    (EOF). Gate: `just test-pty-x86` (`/bin/ptytest` opens a master, forks a slave, and must come
    back from a `poll` on the master when the slave writes). `Pty::select_retry` had no caller at
    all before this, so a master's armed interest was never reported — and `/bin/wterm` drained
    the master in an `EAGAIN` loop instead of polling, which is what hid it. It polls now.

37. **`link(2)` and `rename(2)` send the filesystem no name, and it reads a stale one
    (2026-09-28, FIXED 2026-10-02 — measured).** VFS's `do_link` and `do_rename`
    resolve the names and then pass `core::ptr::null()` where the name goes (`call.rs`'s
    `req_link(src_fs_e, dir_ino, core::ptr::null(), src_ino)` and `req_rename(..., null, ..., null)`), and
    `request.rs` takes the length *from* that pointer (`if _lastc.is_null() { 0 }`), so the
    request carries a zero-length grant and the inode numbers only. MFS has nothing to parse it
    with either: `mfs_main` unpacks the payload into `cch[]`/`user_path` for four requests only
    (26 lookup, 21 mknod, 23 create, 22 mkdir) and no parser exists for `REQ_LINK` or
    `REQ_RENAME`, so `fs_link`/`fs_rename` read `cch[0]`, `cch[1]` and `user_path` from whatever
    request ran before them. That is the class of bug `fs_rdlink` had — it read `cch[0]` for its
    inode — which the readlink work fixed by reading the message in the handler, and which the
    symlink work then avoided by doing the same (`fs_slink` parses its own payload and grants).

    Two independent faults, so the failure need not look like an error. With `cch[]` still zeroed
    the inode number is 0 and the call should fail; but after any create/mkdir/mknod those slots
    hold *that* request's inode and name, and the link or rename is then performed against the
    wrong file under the wrong name. Nothing gates either call — `tools/smoke/` never runs `ln`,
    `mv` or `rename`, and no `/bin/*` client calls `minix_std::fs::link`/`rename` — which is how
    the readlink path stayed stubbed until it got a gate of its own.

    *Measured (2026-09-28).* `/bin/linktest` (`just test-link-x86`) makes its own files in /tmp,
    links one and renames the link, then prints
    `linktest: ln=err 20 tgt=none rn=err 20 mv=none keep=ok` — both calls fail with `ENOTDIR`,
    and the file neither of them names is untouched. The errno is the informative part:
    `ENOTDIR` rather than `EINVAL` means `fs_link`'s `get_inode(dev, dir_ino)` found a *live*
    inode that is not a directory — which is what the previous create's mode looks like in a
    stale `cch[1]` — and `search_dir` refused it before any entry was written. So the
    wrong-directory half of the fault fires first here; that the *name* half is wrong as well is
    still only read from the code.

    The gate was red on purpose until this was fixed: its step is the behaviour the calls
    should have, so the fix flipped it green with no edit to the scenario - the measurement
    that failed and the one that now passes are the same file (`tools/smoke/link.tsv`).

    One side effect worth recording: the probe could not be added to `BOOT_BINS` at first,
    because `/bin` was exactly one directory zone full — 62 programs plus `.` and `..`, 64 slots
    of `4096 / 64` — and `add_dirent` panicked on the next one. Directories now grow into a
    second zone the way MFS's `search_dir` grows them, pinned by
    `a_directory_grows_past_one_zone`.

    Both sides had to be fixed at once, as symlink's were: the resolved name (both names,
    for rename) travels from VFS the way `do_slink`'s does, and MFS reads the message rather
    than `cch[]`/`user_path`. The gate is in `/bin/symlinktest`'s shape - a client that links
    and renames a file it made and reads the result back, each step with its own marker so an
    earlier step's output cannot satisfy a later one (`silent-failure-traps`).
    (`crates/servers/src/vfs/call.rs`, `crates/servers/src/vfs/request.rs`,
    `crates/fs/src/mfs/main.rs`, `crates/fs/src/mfs/link.rs`)

    *Fixed (2026-10-02).* Both sides carry the name now, and finding that was not the whole
    fault. VFS resolves the path to its parent and sends the name's last component as a
    `direct` grant, because the bytes are VFS's own (`req_link`/`req_rename`, the shape
    `req_slink` already used); MFS parses its own payload and copies the name through the
    grant (`fs_link`/`fs_rename`, one `copy_name` for both). With that, the link worked and the
    rename still failed with `ENOENT`, which is the third fault: `advance` assigned `err_code`
    only when the lookup *failed*, where C assigns `search_dir`'s status (`path.c`), so a
    successful lookup left the *previous* request's error behind - and `fs_rename`/`fs_unlink`
    take their whole verdict from `err_code` rather than from the result, so both failed on a
    call whose every step had succeeded. `advance` assigns it either way now.

    Measured: `just test-link-x86` prints `linktest: ln=ok tgt=ok rn=ok mv=ok keep=ok` where it
    printed `ln=err 20 tgt=none rn=err 20 mv=none keep=ok`, and the intermediate state
    (`ln=ok tgt=ok rn=err 2 mv=none keep=ok`) is recorded in `tools/smoke/link.tsv` - which is
    why that gate is whole-line: a fix that carried the names and stopped there would have left
    `ln=ok` to be read as "the calls work". The scenario was not edited.

    Two things are inherited rather than fixed, and worth recording as such. `req_link` carries
    no uid/gid, so `fs_link`'s "only the super-user may link to a directory" check reads whatever
    `caller_uid` the last credentialed request left - which is what C does too (only `req_slink`
    sends credentials), so it is MINIX's shape rather than the port's. And `do_rename` never
    compares the two parents' filesystems, where C returns `EXDEV`; one filesystem is mounted
    today, so that is unreachable rather than live.

38. **The kernel image reaches past the user VA base, so a process's own code shadows its
    embedded initramfs (2026-09-28, resolved 2026-10-01 — measured).** `just test-boot-x86` reports exactly one
    failure, `FAIL: missing /sbin/devman`, and the archive is not corrupt:

    - `initramfs_data()` reports `len=28571180`, which is exactly the cpio (11,793,964) plus the
      16,777,216-byte disk image — `tools/minix-raw.ld` places `*(.minixfs)` *inside*
      `[__initramfs_start, __initramfs_end)`, so the kernel's embedded blobs are one 28.6 MB
      slice starting at `0x596000`.
    - Walking it from the boot test stops at `stop=10920376`, whose VA is `0x10011b8` — that is
      16 MiB **plus 440 bytes**. Every name before that offset resolves (echo, sh, pipetest,
      ld.so, libc.so, dynclib) and every name from it on does not (devman, fb, input, wlserver,
      readlinktest, linktest).
    - The bytes there are neither zeros nor archive: they are x86 code, and the *file* has a
      valid `070701` header at that offset. `0x1000000` is where user programs and servers are
      linked (`/bin/echo ELF entry=0x1000000`), so what the read returns is a *user program's
      text*.

    Nothing writes into the initramfs. The boot test runs on `SYS_BOOT_COMPLETE`, i.e. with the
    calling process's CR3 (VFS's), and that address space maps the user window from `0x1000000`
    up: below it the identity map reads the kernel image, above it the process's own mapping
    shadows it, so the kernel's embedded blobs are unreachable from a process context past
    16 MiB. `boot_init` reads what it needs at boot with the kernel's own tables, which is why
    the boot itself is fine and only the test is wrong — `boot_init` and `boot_test` are the only
    two callers of `find_initramfs_file`.

    The kernel image is `0x200000..~0x20D3000`: text/data/bss to `0x596000` (3.5 MB), then the two
    blobs. Nothing short of removing a blob changes that — the embedded disk image is 16 MiB on
    its own, and without it the cpio still ends at `0x10D6000`, about 875 KB past the boundary.
    It is knife-edge in the other direction too: `/sbin/devman` sits 440 bytes past the 16 MiB
    line, so a few hundred bytes of growth anywhere in the kernel's own text moves the failure to
    a different entry without anything in the archive changing, which is how it appeared here.

    *A has landed (2026-09-28):* the boot test's initramfs checks now run inside `on_kernel_tables`
    (`boot_test.rs`), which installs `boot_cr3` around the lookups and restores the caller's
    tables after — the same switch `exec` uses to reach physical memory through the boot identity
    map, and a no-op on `arch-sim`, whose `boot_cr3` is 0. The gate is green and measures the
    archive again rather than the CR3 it happens to run under.

    *B was attempted and reverted (2026-09-28).* The span split stayed and was committed on its
    own: `tools/minix-raw.ld` gives `*(.minixfs)` its own output section, so `initramfs_data()` is
    the cpio and not the cpio plus the disk image — correct whatever the base is, and the reason
    the original failure's offset was 440 bytes past 16 MiB rather than inside the cpio. The user VA
    base then moved up, 48 MiB rather than the 1 GiB first proposed, because x86's stack is at
    `0x0FE0_0000`, RISC-V's at `0x8FE0_0000` and the AArch64 kernel at `0x4000_0000`, so no shared
    1 GiB slot exists: `tools/minix-user.ld` to `0x0400_0000`, the loader to `0x0700_0000`,
    `ldso`'s `DSO_BASE` to `0x0500_0000` under a `DSO_LIMIT` of `0x0700_0000`, the target spec's
    `--image-base` to `0x4000000` (one `minix_opts`, shared by all three minix targets, so a stage1
    rebuild went with it), and the AArch64 `user_image_base()` constant with its value-based tests.
    All three arches booted, ran and passed their gates with it, and the archive no longer reached
    past the base.

    It was reverted anyway, because it broke a gate and the breakage could not be explained: **a
    dynamic exec's child never reaches the loader's entry**, so `just test-dynlink-x86` and
    `just test-cdyn-x86` hung on the first dynamic program (`/bin/dynclib` in a plain image was
    enough to see it: nothing printed and the shell never prompted). Measured with the copy fix
    below already in place — the kernel reported exec success for the dynamic image
    (`entry=0x7000000`, span `0x3FFF000..0x700B000`, header page `0x3FFF000`), the target's
    `p_rts_flags` was 0 afterwards and it was enqueued, and the entry page *was* absent in the
    fresh table after the clear loop, so the first instruction fetch had to fault — yet no fault
    was ever taken at `0x7000000`, while the sibling static execs faulted at their own entry. A
    marker at the top of `ldso::rtld::run` never printed, so the loader executed nothing. It was
    not the copy: the same hang reproduced with `ipc.rs` at `HEAD`. Reverting the layout makes both
    gates green again, so the hang is the base move's doing and is **hidden, not fixed** —
    `.notes/physmap.md`'s P2 and P4 change the same tables, and both stages should carry "re-check
    dynlink" as a gate. The layout itself now belongs to that plan: moving the user window is one
    decision, made once, with the physmap present, rather than twice (see `.notes/physmap.md` D1–D4).

    **What the attempt exposed is worth keeping, and part of it landed.** The same shadowing works
    in the other direction: this kernel dereferences *physical* addresses as pointers in several
    places, which is sound only while the identity map answers for them, and a user window covers
    physical addresses that boot processes own. Measured: the fb server's `VM_MAP_PHYS` message
    sits on its stack, those stack pages are around `0x46AA000`, and that address is inside fb's
    *own image* window (with the base at 64 MiB: `0x0400_0000..0x04B1C038`, the `.bss` alone 11.6
    MB), so `copy_from_user`'s read of the physical frame landed in fb's image and the message
    arrived all zeros. VM answered the empty request `ENOSYS`, `fb` reported `no bochs-display`
    (its probe was fine — it finds `1234:1111` with `BAR0 0xfd000000`), and the visible symptom was
    one line: `wserver: mmap /dev/fb failed`. The same read is one `mini_send` used to *discard* the
    error of (`let _ = copy_from_user(..)`), which is why the fault announced itself as a server
    answering nonsense rather than as a copy that failed.

    *Landed and kept:* `copy_from_user` reads the caller's own virtual address — this kernel runs on
    the caller's page tables, so the virtual address is what reaches the caller's bytes — and uses
    the walk only to answer "is that page there at all", which is what tells a bad address
    (`EFAULT`) from a good one. `mini_send` refuses a send it could not fetch instead of handing the
    destination whatever the kernel-side buffer last held. Both are right at any base: with the
    image at 16..27.6 MiB, frames in that window — part of the kernel's own image among them — are
    shadowed for exactly the same reason. (Also kept: a comment correction in `syscall.rs`, which
    claimed `exec_create_root` copies the identity map *with user access*; it strips `PG_U`, and the
    wrong reason is what sent this investigation down the wrong path for an hour.)

    **Still open, and the general shape:** the *page-table* dereferences are the same hazard and are
    not fixed. `walk`, `map_page` and `clear_page` reach a table through `table_phys as *const u64`,
    and the tables themselves are allocated around `0xFC00000` (252 MiB) with free RAM running to
    256 MiB — so a table page handed out in `0x0FE00000` (254 MiB, inside *every* process's 1 MiB
    stack window) would poison every walk in the system, silently. `pte_is_valid_phys`
    (`phys < 0x8_0000_0000`) bounds the address but says nothing about whether a user mapping
    shadows it. There are two honest shapes, and `.notes/physmap.md` works the choice through: stop
    dereferencing physical addresses (walk-free user copies, and each page table reached through the
    CR3 that owns it), or give the kernel a physmap at a virtual address no user window covers. The
    reference does the second and a scratch-window version of the first — its kernel is linked at
    `0xF0400000` ("map kernel high for max. user vir space"), `pg_map()` asserts
    `vaddr < kern_vir_start`, `createpde()` maps a 4 MiB window into a reserved `freepdes` PDE
    rather than dereferencing a physical address, and every process carries `p_cr3_v` so page tables
    are never reached by physical address either.

    One more thing the reverted attempt taught: the loader reports its errors on fd 2
    (`rtld::write_bytes`), and nothing in this environment shows fd 2 — `ld.so: cannot load ...` is
    invisible, so a loader that *did* start and fail would look exactly like the silence above. That
    is `silent-failure-traps` material independently of this item.

    *`.notes/physmap.md` now has its first stage landed (2026-09-29).* Each arch has a physmap — x86-64
    `0xFFFF_8000_0000_0000`, riscv64 `0xFFFF_FFC0_0000_0000`, aarch64 `0x8_0000_0000` (low,
    because `EPD1` disables TTBR1 there) — with `phys_to_virt`/`virt_to_phys` as the only
    conversion, compile-time assertions that the base is above every user address, and
    `install_physmap`, which maps the same 0..32 GiB the identity map covers in 1 GiB blocks. The
    boot test writes through the window and reads the frame back through the identity map, and
    the reverse, on all three arches. What it is not yet is *used*: the derefs are P2, and the
    per-process install is P4.

    *And it is used now: `.notes/physmap.md` P4 landed on all three arches (2026-10-01, x86 last).* Every
    per-process table's identity map is the kernel image plus the low frames and stops at the user
    base (x86's `0x200000..~0x20D3000` under a `identity_map_top()` of 64 MiB; AArch64's and
    RISC-V's start above their low-1 GiB windows instead), every page-table frame and every kernel
    buffer that a physical message names is reached through `phys_to_virt`, and the boot test's M1
    assertion — no present kernel mapping inside a user window, and no user-accessible mapping
    inside a kernel VA range — runs on all three arches. So both shapes this item listed as
    "still open" are closed: the page-table dereferences went through `table_ptr` (P2b) and the
    frame dereferences through `frame_ptr` (P3, P4).

    The `[x86]` half of this item is closed too, and **the hang that reverted the base move has a
    cause.** `fpu.rs` kept `p_seg.fpu_state` as a *frame pointer* and dereferenced it at its
    physical address ("The kernel reaches it through the identity map"); once the DSO region
    (80..128 MiB) landed inside RAM, the first dynamically-linked program took an unbounded
    kernel-mode write fault there (`-d int`: `v=0e e=0002 cpl=0`, hundreds of thousands of
    repetitions, RIP at `kernel::fpu::save`). Phase 1 moved all five sites to hold a *physmap*
    address. Nothing is left hidden: `just test-dynlink-x86` and `just test-cdyn-x86` are green
    with the base at `0x0400_0000`, so this item's own lesson — a frame's physical address is not a
    pointer — is what closed it.

39. **`vm_paging_fork` leaked what it built when an allocation fails mid-walk (2026-10-01, FIXED on
    all three arches).**
    `crates/arch-x86_64/src/hal.rs`'s `vm_paging_fork` writes each child table's parent entry as it
    descends and calls `fresh_table()` at three nesting levels; the first failure returns `-12`
    immediately, leaving every frame allocated so far — the child PML4, and any PDP/PD/PT already
    built — unreferenced, with no rollback and no free. The caller is `do_vm_paging_handler`
    (`crates/kernel/src/system.rs`), which on a non-zero result writes 0 into the caller's CR3 field
    and propagates the error, so a partial root is **never installed**: this is a frame leak once
    the guest runs out of physical memory, not a fault.

    It is recorded because an earlier P4 attempt reported a `CR2 == RIP` instruction-fetch fault with
    "a child CR3 whose PD was empty", which pointed here. That attempt's `exec_create_root`
    allocated 32 PDs and returned 0 when one failed to allocate on a 256 MiB guest; the shrink to a
    single PD removes the failure mode, and the fault did not reproduce with it. The fix, when it is
    wanted, is to pre-allocate the tables or free what was built before returning — the
    allocation-failure path is the only one affected.

    *Fixed for x86 (2026-10-01).* A failing x86 fork now runs `free_fork_tables` before returning: the
    walk follows only the entries the fork itself wrote, releasing tables and leaving every leaf
    alone, since a leaf is either the parent's frame (a COW share) or a copied supervisor entry.
    Leaves are also why it tests for a 1 GiB or 2 MiB entry *before* descending: the old code
    allocated the PD for a 1 GiB leaf first and then overwrote the parent entry with the leaf,
    leaking that PD outright -- the new walk is what found it. The root page is the handler's own
    allocation, so `do_vm_paging_handler` releases it with `pagetable::free_pt_page`; that is done on
    all three arches and is correct whether or not the arch's walk cleans up after itself.
    `crates/arch-x86_64/src/hal.rs`'s `fork_release_walk_frees_tables_and_spares_leaves` builds a
    child tree by hand, runs the walk, and requires the released set to be exactly the tables --
    including one reachable only through the kernel half, which must not be visited, and two leaves,
    which must be spared. It needs no allocator, because the one the host suite can see is pointed at
    an address that is not memory (`alloc.rs::test_global`): `release_table_page` records into a
    `#[cfg(test)]` log instead.

    *AArch64 is fixed as well (2026-10-01).* Its walk needs one rule more than x86's, and the same
    rule RISC-V would need: its fork copies the **whole** root first and replaces an entry only when
    it deep-copies the table below it, so an entry the fork has not reached still points at the
    parent's table. An entry is the walk's to release exactly when it differs from the parent's at
    the same slot -- every replacement is a freshly allocated page, so it always differs -- and a
    block entry is a leaf and is never followed. Its existing harness made the rollback checkable
    without new scaffolding: `test_vm_paging_fork_returns_enomem_when_exhausted` now gives the fake
    allocator exactly one page back before the failing call, so the fork's first table is taken and
    the second fails, and requires exactly one page to be available afterwards -- not zero (the
    table was not returned) and not two (more was returned than was taken).

    *RISC-V is fixed as well (2026-10-01), with the same parent-comparison rule.* It could not be
    reached before that: `alloc_phys_page` never returned `None` (item 40), so a fork's `None` arm
    was dead code and no leak could happen through it. With that fallback gone the arm is live. The
    walk lives in a new `crates/arch-riscv64/src/fork.rs` rather than in `hal`, which
    `lib.rs` gates to `target_arch = "riscv64"` -- a test written in the gated module never runs.
    That is the reason AArch64's fork module exists too, and it is why the walk's pointer conversion
    is a `target_arch` seam rather than a `cfg(test)` one: RISC-V's `phys_to_virt` also lives in the
    gated module, so the host has to be given the identity by the same test. Its
    `fork::tests::fork_release_walk_frees_tables_and_spares_leaves` builds both roots by hand and
    requires the released set to be exactly the child's own L1 and L0 -- with a table shared by both
    roots (the split pass writes the same branch into each) that must not be touched, a COW'd leaf
    entry that differs from the parent's and is still not a table, and the parent's own tables.
    `vm_paging_fork` itself is still not host-drivable there (it allocates through a module that does
    not exist on the host), so the three call sites rest on inspection; the walk, which is the part
    with the rules, is covered.

40. **An exhausted RISC-V allocator handed out the same frame forever (2026-10-01, FIXED).**
    `crates/arch-riscv64/src/alloc.rs`'s `alloc_phys_page` ended with

        // Fallback: use a known-safe page outside PMP regions
        // The page at 0x8FF00000 is in RAM, above OpenSBI's PMP regions (0x80000000-0x8004FFFF).
        Some(0x8FF00000u64)

    That was not a fallback, it was an unbounded source of aliases: every call after the bitmap ran
    out returned the *same* physical address, so two page tables -- or two frames of any kind --
    ended up at one PA and the second write silently overwrote the first. It also made every `None`
    arm above it dead, which is why item 39's RISC-V leak could not be reached. `alloc_phys_contig`
    never had such a fallback, so the two entry points agreed only while the bitmap had room.

    *Fixed (2026-10-01): the fallback is gone and exhaustion returns `None`*, with the comment that
    replaced it naming the reason (every caller -- `exec`, `fork`, `map_page` -- already reports
    ENOMEM). Nothing depended on it: a 256 MiB riscv64 guest boots to the shell with 223 MiB usable
    and reports `OK allocator page=0x0000000086498000`, and `just test-arches` and
    `just test-dynlink-riscv64` are green.

42. **On aarch64 a fork child that page-faults clobbered the parent's stack -- FIXED (2026-10-02).**
    Measuring item 16 made `/bin/winprobe`'s children fault for the first time on aarch64 (before the
    fix all four probed VAs were mapped, so no child ever faulted). With the fix, the per-VA
    diagnostic line -- built in a stack array -- reached the console with its first 8 bytes
    overwritten, while a `.rodata` literal and a `.bss` buffer stayed clean. It was not memory
    corruption: the compiler kept the line's prefix constant (`winprobe`) live in **`q8`**, a
    callee-saved SIMD register, across the `fork()` syscall, and aarch64 did not preserve it. Two
    gaps had to close together:

      * The exception frames saved only `q0-q7` and `q16-q31`, on the comment "q8-q15 are
        callee-saved (preserved by the C ABI), so they need no save here" -- true for a call within
        one process, not for the kernel clobbering them.
      * The frame is where a user process's SIMD lives between the asm entry and its return, and a
        context switch *repurposes the frame*: `aarch64_post_syscall` / `aarch64_timer_callback` swap
        only the 288-byte GPR half (`p_reg`) between processes, so the SIMD half stayed whatever the
        previous occupant left. A resumed process therefore ran with another process's `q0-q31`; only
        `q8-q15` mattered, because that is where the compiler may keep live values across a call
        (caller-saved `q0-q7`/`q16-q31` it spills around one).
    Disassembling `winprobe` pinned it: `mov`/`movk x29, "winprobe"` hoisted out of the loop,
    `str d8, [sp, #0xd0]` in it, and a `cmp x8, x29` self-check that read back the child's bytes. This
    is the aarch64 form of item 12 (the x86 `XMM` clobber) and the class `fpu.rs` exists for; its
    `save`/`restore`/`fork_inherit` were no-ops on aarch64, on the assumption that the exception frame
    covered the FP state.

    *Fixed (2026-10-02).* Three changes:
      * `arch-aarch64/src/exception.rs`: the three exception frames (`el1_sync`, `el1_irq`,
        `el0_sync`) grow from 672 to 800 bytes and save/restore `q8-q15` next to the caller-saved
        block, so all 32 `q` registers survive one process's own kernel entry.
      * `kernel/src/fpu.rs`: `frame_save`/`frame_load` copy the frame's SIMD block (offset 288, 512
        bytes) to and from a per-process `p_seg.fpu_state` page; the aarch64 arms of `reset` (exec)
        and `fork_inherit` are implemented too.
      * `kernel-boot/src/aarch64.rs`: `aarch64_post_syscall` and `aarch64_timer_callback` swap the
        SIMD block alongside `p_reg` on every switch, and the exec path zeroes it for the new image.
    Verified: `just test-winprobe aarch64` prints all four diagnostic lines clean across repeated
    runs and `probed=4 reachable=0`; `just test-arches` (12 gates) is green. `tools/smoke/winprobe.tsv`
    now also asserts a per-VA diagnostic line, so a regression fails the gate instead of only
    corrupting the log. The boot entry does not go through a frame, so `switch_to_user`'s caller now
    restores the target's SIMD image itself (`fpu::restore`'s aarch64 arm, zeroed for a process that
    has not run), closing that last leak; the frame's SIMD block was also relaid out in `q0..q31`
    order so a verbatim copy to the per-process area is linear.

43. **~~A process whose worker threads exit never returned from `thread_join`~~ (FIXED
    2026-11-09).** The SIGSEGV that stopped `/bin/hello` was not a bad address: VM read bit 0 of the
    page-fault error code as "the access was a store", but the code has x86_64's layout on every
    arch, where bit 0 says the page was present and bit 1 says the access was a write. A protection
    fault on an instruction fetch therefore demanded `VR_WRITABLE`, `.text` has no write permission,
    and the handler killed the process for running its own code. The fetch is legitimate: hello's
    last `.text` instruction is a 6-byte `jb` starting at `0x4039fff`, whose remaining four bytes are
    on page `0x403a000`, so all four worker threads fault on that page; the first starts the FDIO
    fill and every later one fetches the supervisor-only placeholder, which the misread bit turned
    into a SIGSEGV. Fixed in `fa5d71a66`: `crates/servers/src/vm/mod.rs` decodes present and write
    separately, gates copy-on-write on both, and a host test pins `0x15` as a present fetch that is
    not a store. Three consecutive `/bin/hello` runs in one x86 guest print all four thread lines
    and `threadstd: all joined, shared=4`, and the shell takes another command afterwards.

    Three readings in this item's open form were wrong and are recorded so they are not repeated:
    `mp_flags = 0x69` with no `ZOMBIE` did not mean `exit_proc` stopped before `zombify`, because
    `tell_parent` clears `ZOMBIE` and sets `TOLD_PARENT` by design; the seven `e=0x15` faults were
    seven SIGSEGVs coalescing into one pending signal, not a thread parked seven times; and
    `stop_proc`'s ptrace `T_STOP` was not the wedge, since the exit reached `tell_parent` and the
    slot is released by the parent's reap, which needs no `T_RESUME`. The `0x3bca0` that looked like
    a `filesz` mismatch is `p_offset + p_filesz` (`0x1000 + 0x3aca0`), the absolute in-file end that
    `finish_page` compares against. (`crates/servers/src/vm/mod.rs`,
    `crates/arch-riscv64/src/faultcode.rs`)

44. **~~A name longer than 28 bytes could not be created~~ (FIXED 2026-11-09).** The item's
    premise was stale: `create`, `mkdir` and `mknod` already carry the name in a grant on both
    sides (`req_create`/`req_mkdir`/`req_mknod` in `vfs/request.rs`, MFS's dispatch arm and
    `parse_mknod_request`), and 29- and 40-byte names created and read back. Three defects past
    that were real. The shell's `try_exec` packed every argument into the command-path buffer and
    clamped each one to 55 bytes, so `/bin/echo` with a 61-byte argument printed 55; arguments
    now have their own buffer, sized from the input line (`pack_args`). With that fixed a 60-byte
    name created and opened by name, but `ls` showed it one character short: `fs_getdents` read a
    name that fills the 60-byte on-disk field as `MFS_NAME_MAX - 1`, though such a name has no
    terminator rather than ending early (`entry_name_len`). And a 61-byte name was truncated to
    60 by the directory-entry write instead of being refused, so creating one landed under a
    different name; MFS's dispatch now answers `ENAMETOOLONG` when `path_len` exceeds
    `MFS_NAME_MAX + 1`, and `errstr` names code 36.

    Measured in one x86 guest: 29-, 40- and 60-byte names each create, read back by name and list
    at full length; a 61-byte name is refused with `mkdir: <name>: err=36 File name too long` and
    leaves the existing 60-byte file untouched; a 61-byte argument reaches `/bin/echo` whole.
    `tools/smoke/long-path.tsv` covers the whole range now (it had stopped at 24 with a comment
    deferring this), 26 steps green, and `just test-arches` (all sixteen gates on three arches)
    plus the host suites (`fs` 304, `userland` 79, `servers` 758, `kernel` 426) pass.
    (`crates/userland/src/shell.rs`, `crates/userland/src/lib.rs`,
    `crates/fs/src/mfs/protect.rs`, `crates/fs/src/mfs/main.rs`, `tools/smoke/long-path.tsv`)

45. **`execve` wrote past a 64-entry offset table for a long argv/envp sequence
    (fixed 2026-11-17).** The argument frame is built around `offsets: [0usize; 64]`,
    filled once per argv entry and then once per envp entry, but each scan was capped at 63
    independently (`argc < 63`, `envc < 63`), so the pair could make 126 writes through a
    64-slot array: a 63-argument command with two environment variables corrupted the stack
    around the frame. It is on the path an exec failure (item 38) would traverse, which is why
    it was inspected. The table is now `2 * MAX_EXEC_ENTRIES` (126), a const assertion keeps it
    at least that large, and a scan that stops at the cap with entries remaining returns
    `E2BIG` instead of silently truncating argv or envp. Pinned by
    `exec_offset_table_covers_the_largest_argv_and_envp`.
    (`crates/minix-rt/src/lib.rs`)

46. **~~The C header's `O_*` flags did not match MINIX's~~ — FIXED (2026-11-17; was open
    item 36).** The header, `minix-libc`, `minix-std`, the kernel's `SYS_open` check,
    VFS's `do_open`/`do_creat`/`do_pipe2`/`eventfd`/`timerfd`/`epoll`, the DRM
    `DRM_CLOEXEC`, and the std PAL's duplicated constants now all use MINIX 3.3.0's
    `sys/sys/fcntl.h` values (`O_CREAT` 0x200, `O_EXCL` 0x800, `O_TRUNC` 0x400,
    `O_APPEND` 0x8, `O_NONBLOCK` 0x4, `O_CLOEXEC` 0x400000). The open flags are defined
    once in `vfs::consts`, the std PAL re-exports `minix_std::fs::O_*` instead of keeping a
    copy, and `do_open`/`do_creat` now honour `O_CLOEXEC` at open time (C's `common_open`
    does), with `do_open`'s stale `1 << 12` guard replaced by the flag. The stage1 sysroot
    was rebuilt (`just bootstrap all`) so std carries the new values. Gates:
    `cargo test --workspace` (3203 passed), clippy, `just check`, `just test-cabi x86`
    (the `pipe2(O_NONBLOCK)` probe), `just test-cdyn-x86`, and
    `just test-coreutils-wedge` (`coreutils seq 3 > file`, the std `File::create` path).
    The Mesa/gbm/EGL probes were not re-run: the prebuilt DSOs predate the header and a
    source build needs WSL.

    Still the port's own ABI: `F_GETLK`/`F_SETLK`/`F_SETLKW`, the lock types and
    `struct flock` in the same header, and `minix-libc::fcntl` takes an `int` third
    argument, so a lock command cannot carry a `struct flock`. That is a separate defect,
    recorded here rather than silently aligned; the lock path stays reachable only from a
    message a caller builds itself.
    (`tools/c-include/fcntl.h`, `crates/servers/src/vfs/consts.rs`,
    `crates/servers/src/vfs/call.rs`, `crates/servers/src/vfs/device.rs`,
    `crates/servers/src/vfs/{eventfd,timerfd,epoll}.rs`, `crates/minix-std/src/fs.rs`,
    `crates/minix-libc/src/{lib,c_stdio,c_stdlib}.rs`, `crates/kernel/src/syscall.rs`,
    `crates/drivers/src/video/drm.rs`,
    `rust/library/std/src/sys/pal/minix/syscall.rs`)

47. **~~A cache-evicted `MAP_SHARED` page diverged from its mapping~~: FIXED
    (2026-11-17).** The file page cache holds one `PhysBlock` reference per page and a
    mapping holds its own, so `insert_entry`'s eviction used to drop the LRU entry even
    when its count was above one: the mapping kept the frame, the file key was gone,
    and a later fault of the same `(dev, ino, offset)` read the file into a new frame,
    so two mappings of one page stopped sharing. Eviction now walks the LRU
    oldest-first for an entry whose count is one (the cache's alone) and leaves a
    mapped page in place; when every entry is mapped the new page is simply not
    cached, and the refused insert takes no reference. Pinned by
    `test_a_mapped_page_is_not_evicted` and
    `test_a_full_cache_of_mapped_pages_refuses_the_insert`, and the existing
    `test_lru_eviction` now exercises cache-only pages. Writeback, the other half of
    the original gap, is fixed too (item 48).
    (`crates/servers/src/vm/cache.rs`)

48. **~~A `MAP_SHARED` file page had no writeback~~: FIXED (2026-11-17).** A store
    through a writable shared file mapping lands in the cache frame every mapper
    shares, and nothing traps the store, so the file itself stayed as it was: closing
    and reopening lost the write. The cache now carries a dirty mark per page
    (`vm/cache.rs`), set when a writable shared region is released and cleared once
    its writeback lands; a dirty entry is never evicted, for the same reason a mapped
    one is not. `vm/writeback.rs` owns the asynchronous side: `munmap`, `exec` and
    process exit mark the released region's file range and queue a job that walks the
    dirty pages and sends `VMVFSREQ_FDWRITE` for each. The bytes come from a mapping
    of the cache frame in *VM's* address space, held with a `PhysBlock` reference
    until VFS answers, because the mapping process's address space can be gone before
    VFS reads it. The vmfd is closed by the last job queued for it, after that job's
    writes, which ordering VM's FIFO async queue at VFS provides. VFS's handler
    (`do_vm_call`) writes through `req_write` and clamps the length to the vnode's
    real size, so a file that ends mid-page is not extended. Only a writable shared
    *file* region is written back: a memfd's frames are the object, and a read-only
    mapping cannot have been written through. Still owed: `msync` does not exist as a
    syscall in this port, and a shared region `mprotect`ed read-only after a write is
    not written back (nothing in the tree `mprotect`s a file mapping). Gate:
    `just test-writeback <arch>` (`tools/smoke/writeback.tsv`) creates a file, maps it
    shared, has the child store through the mapping and exit, and then, after the
    parent verifies its own view, reopens the file with a new fd, which cannot see the
    cache; on the old tree it fails with `shared write did not reach the file`.
    Pinned on the host by `test_mark_range_dirty_covers_only_that_file_range`,
    `test_find_dirty_after_walks_offsets_in_order`, `test_a_dirty_page_is_not_evicted`,
    `test_mark_written_keeps_a_still_mapped_page_dirty`, and the writeback module's
    FIFO, ring-full and region-filter tests. `just test-arches` is green.
    (`crates/servers/src/vm/{cache,writeback,vfs_request,mod}.rs`,
    `crates/servers/src/vfs/{call,consts}.rs`, `crates/arch-common/src/com.rs`,
    `crates/userland/src/lib.rs`, `tools/smoke/writeback.tsv`, `Justfile`)

---

## x86_64 (`[x86]`)

- **Piped serial input loses bytes under bursts, on the host's `-nographic` transport rather
  than in the guest (measured 2026-11-09).** QEMU's stdio chardev pushes a pipe write as one burst,
  and a 240-byte line can lose a run of characters (1, 4, 13 and 48 measured; earlier:
  `chmod 4755 /bin/sugid` -> `chmod 4755 /bingid`) while short lines and interactive typing are
  fine. It is the transport, not the guest: the same image and the same single write over a socket
  chardev (`-serial chardev:socket` in place of `-nographic`) delivered 96 of 96 bursts whole,
  while `tools/smoke/feed.sh` over stdio lost three of 120. That exonerates the 16550 FIFO and its
  ISR trigger (14 on x86, 1 on RISC-V), the syscall-entry drain and the `ser_input` ring on this
  evidence, and it means the earlier FIFO-overrun reading of the same symptom was wrong. A probe
  that must see every byte still paces its input (`FEED_PACE=0.003`, or `su_setuid_probe.py`'s own
  ~3 ms gaps), because a guest-side change cannot repair a drop in the host path.
  (`tools/smoke/feed.sh`, `target/sock_burst.py`-style probe)

  *The 2026-11-09 "rotated" measurement was not a rotation of one stream: a second console
  line was stealing the burst, and that is fixed.* `tty_init` wires `console_read` as the devread
  hook of all four console lines, and the notification pump runs `handle_events` on every line, so
  every console line drained the single kernel serial ring. The lines that are not the active
  console keep the default termios (`ICANON | ECHO`), so a line that took the tail of a burst
  **printed it** through its own discipline. Measured with `/bin/echo ` and 230 distinct characters
  in one write: the console showed the argument's tail (the stealing line's echo), a line break
  (that line's echo of the stolen newline), then the command prefix and the argument's head (the
  shell's editor echoing what it did receive), and the shell never saw the newline so it never ran
  the line. The split point differed per run because it is where the pump moved on to the next
  line. `takes_serial_input` now lets only the active console line (`CONSOLE_LINE`) take bytes from
  the ring. Before it, three rotations in the reproducer; after it, none. The reproducer is now a
  tracked gate on all three arches, `just test-burst` (`tools/smoke/burst.tsv`, in `test-arches`):
  three 240-byte lines typed in one write, each argument tagged so a byte lost to the stdio
  transport still passes while a line the shell never ran fails.
  (`crates/servers/src/tty.rs`, `tools/smoke/burst.tsv`, `Justfile`)

- **A pending notification starved the async reply that finishes a file page, so `/bin/hello`
  wedged with its text page left as a fill placeholder (fixed 2026-11-09).** After the thread and prompt work,
  `/bin/hello` stops after `alloc: churn + realloc + align ok`: the four worker lines never
  print, the two worker threads sit in `RTS_PAGEFAULT` at the closure entry (`0x40285d0`), and
  the process ends with `JOINING | P_STOP | SIG_PENDING | SIGNALED` on its main slot, so the
  pending SIGSEGV can never be delivered and the shell waits forever (a second `/bin/hello` is
  never even typed to a listening shell). Measured through the QEMU monitor, with no prints in
  the guest: `PAGE_FAULT_INFO[23] = {0x40285d0, err=0x15, valid=1}`; hello's CR3 (`0xfee9000`
  at `Proc+0x100`) maps `0x40285d0` to `0xe85a063`, present, writable, supervisor-only, which
  is exactly the `MAP_WRITE`-only fill placeholder `start_file_page` maps; `-d int` shows one
  not-present fetch (`e=0x14`) and then present-protection fetches (`e=0x15`)
  forever, about 6000 a second. VM's request node for that page carries `reqid 83`,
  `page_addr 0x4028000`, `pa 0xe85a000`, `file_off 0x29000`, `file_size 0x3bca0`; VFS answered
  that request with `result 0` (read from `VFS_GLOBAL.fs_m_out`); and the page is absent from
  VM's file cache (`CACHE_TABLE`, 87 entries, none with offset `0x29000`), so `finish_page`
  never reached its cache insert. `pt_fill_then_finish` (`crates/kernel/src/tests.rs`) maps a
  page both ways on the same address and passes on x86, RISC-V and AArch64, which rules
  `map_page` out and leaves the `VM_PAGING_MAP` kernel call (the `vm_map_page_in` wrapper, or
  whether `finish_file_page` runs at all) as the suspect layer. `start_file_page` now refuses to
  read a non-user page as finished, so a lost fill fails the process instead of livelocking it,
  but the fill is still lost.

    *Fixed (2026-11-09).* The reply was not lost, it was never collected, and the mechanism was
    the receive order. `mini_receive` checked pending *notifications* before pending async
    messages and returned with a notification; VM is notified for every page fault it has not
    resolved, so every receive it made found one and the queued `VM_VFS_REPLY` was never looked
    at. Measured through the monitor: VFS's table entry for `reqid 83` was still `AMF_VALID`
    after the guest stopped, VM's `s_asyn_pending` bit for VFS was set in 60 of 60 samples, VM was
    never `RECEIVING`, and its `rip` sat in `sys_vmctl_memreq_get` (the fault-notification scan).
    A notification is a wakeup and stays pending until delivered, while an async message is an
    answer the receiver is waiting for, so `mini_receive` now tries pending async messages first.
    Pinned by `async_beats_notify` (`crates/kernel/src/tests.rs`), which fails on the old order in
    all three of its checks. After the fix the page maps as `0xe85a025` (present, user-only,
    executable), `finish_page` inserts it into the file cache and VFS's async table drains to
    empty. The remaining `/bin/hello` hang is open item 37, a different defect.

- **Nothing else outstanding in-tree.** The repeated-exec leak cascade (fdref
  self-reference, VM self-map VA march, exec-addrspace reclaim, munmap
  walk) is fixed: `exec_loop_mem.py 400` is flat (leak 0.0 KiB/exec), and
  the 100× degradation no longer reproduces. x86 is the best-tested port.

- `[env]` **QEMU SYSRETQ SS.RPL quirk** — QEMU loads SS with the STAR-MSR
  selector without setting RPL=3; the timer ISR's pop-and-rebuild of the
  IRET frame works around it. Platform limitation, workaround in place,
  documented in the `bare-metal-debug` skill.

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
   change it). Both are QEMU-side; probes must pace input (~3 ms/byte), and
   the failure mode is documented in the `bare-metal-debug` skill.

2. `[env]` **Windows 4 KiB pipe-buffer TX stall** — if a harness captures
   QEMU stdout without draining, the 16550 TX stalls and a blocking
   `putchar` busy-wait looks like a boot hang. Always drain stdout.
   (documented in the `bare-metal-debug` skill; a harness/platform limitation)

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

6. **~~riscv64 ticked no clock at all, and its C heap leaked~~ — FIXED (2026-09-25).**
   Found by driving bash in a riscv64 guest: three defects, all fixed, plus a
   residual flake that is not.
   *The timer was armed but never enabled.* `clint::init_timer` wrote `stimecmp`
   and nothing ever set `sie.STIE`, so the deadline lapsed in silence — no clock
   tick, no virtual timer, no `alarm`, nothing preempted, and
   `kernel::bootwatch` (500 ticks of slack for the userspace boot phase) never
   reached its deadline, which is why `just test-boot riscv64` failed. Compounding
   it, the per-tick accounting sat *behind* the callback's SPP check, so it ran
   only for a tick taken in U-mode — and every tick this port takes arrives in
   S-mode (measured: sampling the 1st, 100th, 1000th and 10000th tick). The
   accounting now runs for both, and the SPP check still decides whether the tick
   may switch. Verified: `just test-boot riscv64` prints `ALL TESTS PASSED`,
   where before it stopped at `phase 1 complete; releasing init`.
   *The C heap leaked.* `realloc` took a block's size from the header at the
   *user* pointer instead of `HDR` bytes below it, so a grow that read high
   returned the same too-small block and the caller overran it; and `malloc`'s
   extension path placed a fresh block at a page-rounded `sbrk` address while
   leaving the free-list walk unable to cross the gap in front of it, so every
   extension leaked its page (`free`'s O(heap) walk then turned the leak into a
   crawl). Measured in the guest: a probe growing one buffer to 100000 bytes
   prints `REALLOC ok`; a 300000-cycle varying-size churn probe goes from hanging
   to `CHURN ok 300000`; `bash -c 'for i in {1..4000}; do s=$((s+i)); done; echo
   S=$s'` goes from killed by SIGSEGV every time to `S=8002000`.
   *The last of it was a trap-state bug, not a C one.* After those fixes a
   `bash -c` step still died in about 5-10% of invocations: the child ended as a
   signal (139 from the shell's `waitpid_status`) and VM's trace of the kill said
   `pf-noregion 0x0` with error code 0x14 — a user *instruction* fetch at address
   zero, which reads as a call through a null function pointer. It was not one.
   The riscv64 trap handler read `stval` **live**, and `stval` is the one fault
   CSR that is not part of the interrupted context: the trap entry saves `sepc`,
   `sstatus` and `scause` into the frame but not `stval`, and the architecture
   lets a trap that is not a load/store/instruction fault — an interrupt, and
   this port takes its ticks *inside* a U-mode trap's handler — write 0 there.
   Measured with the entry's value kept alongside: the live CSR read 0 while the
   saved one held the real address (`cause=0xc`, `0x103257e`, inside bash's own
   text). VM was therefore handed address 0, found no region covering it, and
   killed a process that had done nothing wrong. `trap_asm.rs` now saves `stval`
   into the trap frame's unused `288..296` slot (the same offsets in `hal.rs` are
   the *sigframe*, a different 296-byte buffer on the user's stack), and `trap.rs`
   reads the fault address from the frame, as it already did `scause`/`sepc`/
   `sstatus`. Verified: 60 consecutive `bash -c` invocations in the prompt-paced
   census with zero losses, `just test-bash riscv64` 3/3 green, and
   `test-qemu`/`test-boot riscv64` plus the workspace suite green. Two things to
   carry forward: the timer fix above is what took the arithmetic step from
   failing two runs in three to not failing at all, and the zero that VM saw was
   a plausible-looking NULL dereference until the frame's copy showed the real
   address — the fault *address* a handler forwards is as much state as the
   cause.

   *The FP image, and the one measurement the first probe missed.* The trap frame
   saves no `f`-register and the port kept no per-process image, so with
   `PSL_USERSET`'s `FS=Dirty` (the registers, not the hardware's lazy
   `FS=Off`-and-trap path) two processes each using floating point could see each
   other's registers across a switch. Measured: a probe that names `fs0`, holds a
   value in it across a `getpid()` and checks it afterwards reported `FPPROBE
   parent bad=40000` and `FPPROBE child bad=40000` — every round, both processes —
   while the same probe with the `fork` removed reported `FPONE bad=0`, so the loss
   needed the other process to have run, not merely a trap. A first version of the
   probe passed instead: `register double held asm("fs0")` around the call compiled
   to `fsd fs0` before it and `fld fs0` after it, giving the value a stack home the
   compiler reloaded, so the register never really held it; the set, the call and
   the check are one `asm` block now. The fix saves at the *switch* rather than the
   entry, because the kernel executes no floating point itself — nothing between a
   process's last user instruction and the switch can disturb its registers — so
   `riscv_post_syscall`, `riscv_timer_callback` and the boot `switch_to_user`
   (`crates/kernel-boot/src/riscv64.rs`) call `kernel::fpu::save` on the process
   being left and `restore` on the one being entered. `fork` saves the parent
   before copying the image to the child, `exec` zeroes it, and `restore` writes
   `fcsr` (rounding mode and accrued flags) as well as `f0`-`f31`, so neither leaks
   either; `fcsr` lives past the 256 bytes of registers in the one-page area
   (`RISCV_FCSR_OFF`), while the user-visible `mc_fpstate` stays registers-only.
   Verified: the probe reports `FPPROBE ok` (both `bad=0`), 80/80 steps in the
   prompt-paced census with zero losses, `just test-bash` 6/6 on all three arches,
   `test-qemu`/`test-boot riscv64` green, and the workspace suite green.

37. **~~A 240-byte write overflows QEMU's receive FIFO before the guest drains it~~ — a
   host-transport drop, not a FIFO overrun (2026-11-16; resolved 2026-11-17).**
   `just test-burst-<arch>` types a 240-byte command and failed about one run in two, with the
   guest's echo cut short and then no output at all. That is the host's stdio chardev dropping
   bytes, not the guest's UART: the same image and the same single write over a socket chardev
   delivered 96 of 96 bursts whole, while stdio lost three of 120 (the `[x86]` resolved entry
   has the measurement). The receive FIFOs were already enabled, contrary to this entry's
   premise: x86 writes `FCR` `0xC7` (enabled, cleared, 14-byte trigger) in `init_serial`,
   RISC-V writes `0x07` in `uart::init_uart`, and aarch64 sets `LCR_H.FEN` in `uart_init`. A
   guest cannot buffer an instantaneous host write into a 16-byte FIFO, so the gate paces its
   input (`FEED_PACE=0.003`, one byte per write, `type_step` refusing a truncated echo), as
   `tools/smoke/feed.sh` is built to.
   (`crates/kernel-boot/src/main.rs`, `crates/arch-riscv64/src/uart.rs`,
   `crates/arch-aarch64/src/hal.rs`, `Justfile`, `tools/smoke/feed.sh`)

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
   and the flat exec loop. That is `just test-fork aarch64` now, and it runs in `test-arches`,
   so the claim is checked rather than remembered.
   (`crates/arch-aarch64/src/fork.rs`, `crates/servers/src/vm/cow.rs`)

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

5. **~~Least-tested port: standing gotchas~~: captured in the `aarch64-debugging` skill
   (2026-11-17).** The three traps this entry carried are procedures, not defects, and
   the policy in this file's header puts a skill-captured limitation here: re-derive
   every probe address after a rebuild (skill: "Re-derive every probe address after
   each rebuild"), verify the embedded initramfs matches the on-disk binaries before
   suspecting the kernel (skill gotchas 2 and 3), and check for silent HAL stubs that
   fail kernel calls (skill gotcha 8, which records the `vm_paging_fork` stub that hung
   every fork). None of the stubs remain in
   `crates/arch-aarch64/src/hal.rs`, and the pattern is the skill's to keep.

6. **~~Per-process table layout is dual~~: documented in the `aarch64-debugging` skill
   (2026-11-17).** Both layouts and the consequence for a generic walk are skill
   gotchas 9 and 10: boot procs carry PUD[1] as a PMD table of 2 MiB blocks, exec'd
   processes carry a 1 GiB block plus the low-GB alias PMD, block entries are shared
   verbatim while table entries are deep-copied, and the low-GB alias window must come
   from the cached kernel geometry (`VM_PAGING_MEMINFO`), because VM's own copy of the
   arch allocator is not initialised. Gotcha 10 records the exec-2 hang that this
   class of bug caused. The code that must honour it is `pte_user_owned` and the
   teardown walks, gated by `just test-boot-aarch64`.

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
   of this shape — see `.notes/porting_plan.md` finding 19.

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

9. **~~`/bin/coreutils` is built but not embedded on aarch64: it loses output~~: FIXED
   (2026-11-17).** The loss was the same class as item 42's winprobe corruption: the
   exception frames saved only the caller-saved `q0`-`q7`/`q16`-`q31`, so a context
   switch left the callee-saved `q8`-`q15` holding the previous process's values, and
   the multicall's allocation-heavy startup kept a live value there across a trap.
   `976981af8` saves `q8`-`q15` and swaps the frame's SIMD block with the per-process
   image on every switch; its date (2026-10-02) is after this entry's measurement, which
   is why the entry was stale. The image now carries the multicall on aarch64:
   `crates/kernel/build.rs` and `mkminixfs` no longer skip it, `build-aarch64` depends
   on `coreutils-aarch64` and raises the embedded filesystem to
   `MINIXFS_BLOCKS=8192` (32 MiB, because the AArch64 userland plus the 6 MB multicall
   does not fit 16 MiB), and `just test-coreutils-wedge-aarch64` runs the same scenario
   in `test-arches`. Verified: 14 consecutive aarch64 boots of
   `tools/smoke/coreutils-wedge.tsv` green (`seq 3`, `seq 200 > file`, `wc -l`, `pwd`,
   `WEDGE-OK`), where the entry recorded three consecutive failures.
   (`crates/arch-aarch64/src/exception.rs`, `crates/kernel/src/fpu.rs`,
   `crates/kernel-boot/src/aarch64.rs`, `crates/kernel/build.rs`,
   `crates/boot-image/src/bin/mkminixfs.rs`, `Justfile`,
   `tools/smoke/coreutils-wedge.tsv`)

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
  write reaching the child (item 27). It is a `BOOT_BINS` entry, so every image carries it and
  it runs as a normal command (shell fork + exec); `just test-fork <arch>` is the gate, and it
  runs in `test-arches`. Before that entry existed the only way to reach it was
  `MINIXFS_EXTRA=/bin/forktest=...`, which is why no recipe named it.
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
- **A probe predicate that ends in `\n` never matches the guest's console
  (2026-11-08).** The serial console runs with ONLCR, so every `\n` a process
  writes leaves the tty as `\r\n`. The first version of `tools/wldemo_probe.py`
  waited for a whole line including its terminator and timed out against a log
  that visibly contained that line. Match the content and not the newline (or
  strip the `\r`), and expect the same from any check of a captured guest log
  written on a Windows host, where the file keeps the CRLF.
- **A program the `/etc/rc` list starts writes on top of the shell's prompt
  (2026-11-08, fixed in the shell).** `/bin/rc` forks each line and then execs
  `/bin/sh`. `/bin/wlpanel` started from that list, and its first line of output
  (`wlpanel: ready`) arrived after the shell had printed its prompt but before it
  printed a newline, so the console showed `# wlpanel: ready`. A prompt with text
  after it is not a prompt: `tools/smoke/feed.sh` waits for a line that is exactly
  `# `, so every tsv gate refused such a boot as "no prompt", and that is how it
  surfaced. The same boot also exposed a start race: with the panel as the list's
  first line, two boots produced no panel output at all, and 16 `getpid` round
  trips between forks (`rc::settle`) made six of six boots start it. PM and VFS
  each run one request at a time, so a child still inside `exec` when `rc` forks
  again is the likely loser; that half is mitigated, not explained.

  The first fix was a session manager (`/bin/session`) that waits for the prompt,
  writes the newline the shell did not, and only then starts `/etc/session`. That
  is a race, not a fix: the manager's own delay makes the newline arrive at some
  arbitrary moment after the prompt, and the *programs* keep writing whenever they
  have something to say, so their lines can still land inside a prompt. Measured
  on riscv64: the panel's ready line was split by the shell's next prompt
  (`wlpanel: ready pid=# 23`, every write a separate syscall), `test-fbmmap` lost
  its prompt, and the console was garbage.

  What holds is the tty keeping a line to its writer. Every console line has an
  owner: the process that opened it and left it without a terminator. A write from
  another process starts on a line of its own, so the shell's prompt stays `# ` with
  the cursor after it, the editor's echo of what is typed shares the shell's
  endpoint and is untouched, and the panel's line is pushed down. The prompt stays
  inline, which is the only shape a CLI has.

  That needed a wire fix as well: VFS sends the *writer's* endpoint in `m2_i3` of a
  `CDEV_WRITE` (the same field a read carries a reader in, and the id VFS matches
  the reply by), but `do_write` stored the *sender*, which is VFS for every write,
  so all writers looked alike and nothing could tell them apart. `tty_outcaller`
  is the process now; `tty_outgrant` went with it, because the write path carries
  no grant (the chunk is inline in `m2_l3`). The session manager keeps its delay
  for the `exec` overlap only.

  What an interleaved write used to cost, the prompt being pushed up and the command
typed at it left on its own line, is gone as well. The tty counts the takeovers
(`tty_line_taken`, read with the port's own `TIOCGTAKEN`, group `T` rather than a
NetBSD request). The shell stores the count when it draws the prompt and compares
it before acting on each byte it reads; a change means the row it was editing now
belongs to something else, so it reprints `# ` and the text typed so far on a fresh
row. The first command after boot, typed while the session's two lines are on the
screen, therefore still appears after its prompt.

  Gates: `tools/rc_probe.py`, which types nothing and looks for the start line, the
  `# ` prompt as a line of its own, and the panel's ready line, plus every tsv
  scenario, which needs the `# ` prompt.
  (`crates/servers/src/tty.rs`, `crates/userland/src/shell.rs`,
  `crates/minix-std/src/termios.rs`, `crates/userland/src/rc.rs`,
  `crates/boot-image/src/manifest.rs`)
- **A surface's pixels and its hit box came from two different places
  (2026-11-08, fixed).** The server works out where an anchored layer surface
  sits (`wayland::server::place`) and the scene carries it, which is what
  `hit_test` and the focus used. `present` did not read it: a shm commit that was
  not the whole output was blitted at its damage rectangle's own origin, so every
  such commit landed at (0, 0). A toplevel is at the origin, so no window gate
  could see it, and `tools/wlpanel_probe.py` clicked the output's right edge and
  got an answer from a panel drawn in the top-left corner. `present` and
  `present_dmabuf` now take the surface's place and offset the blit by it, and
  the panel probe reads the display itself (a QMP screendump: the strip at the
  right edge, the boot pattern's red still at the origin) so the two cannot part
  again. (`crates/servers/src/wlserver.rs`)
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
  heap base as a literal (`0x3FE00000`, which was x86_64's alone — aarch64 is at
  `0x2000_0000`, and riscv64 moved there with `.notes/physmap.md` P4), so `just test-qemu-aarch64` failed at
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
  `tools/ccflags.py`, shared by `tools/build-c-hello.py` and `tools/cc-minix.py`. A
  missing header is a hard error now, which is the point. What it fixed, in
  order: `_POSIX_VERSION` had to exist for bash to stop using `union wait`;
  `HAVE_TERMIOS_H` answers no; `clock_t` needed a `sys/times.h` to be found
  (bash writes `#define clock_t long` otherwise, which collides with the host's
  typedef in the build tools); and `locale_utf8locale` needed `wcwidth`, without
  which `HANDLE_MULTIBYTE` is off while bash's globbing still uses an identifier
  only the multibyte branch declares. `.notes/c_build.md` has the write-up.
  (`tools/ccflags.py`, `tools/build-c-hello.py`)
- **~~What the hermetic compile then exposed: the port's own missing headers~~
  — FIXED (2026-09-24).** With the host out of the picture the bash build's
  errors became a to-do list, and every entry is closed now: `termios.h` and the
  tty surface behind it (`tcgetattr`/`tcsetattr`/`tcgetpgrp`/`cfmakeraw`, …),
  `sys/ioctl.h`, `sys/param.h`, `utime.h`, `netinet/in.h`/`arpa/inet.h`,
  `mktemp`, `mknod`/`mkfifo`, `times`, `bsearch`, `wcwidth`/`mbstowcs`, and
  `_POSIX_VERSION` in `unistd.h`. `.notes/c_build.md`'s "the missing terminal
  interface" list is the inventory, and bash configuring, compiling and linking
  at all is the check on it (`just build-bash`).
  (`tools/c-include`, `crates/minix-libc`)
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
- **bash is a committed build with a gate (2026-09-25).** `tools/build-bash.py`
  fetches bash at a pinned upstream commit, rebuilds `minix-libc` with this
  host's stage1, configures against `tools/c-include`, links with
  `tools/cc-minix.py` and publishes `target/bash/<arch>/bash`. On Windows the script
  re-enters WSL by itself (`MINIX_WSL_DISTRO` picks the distribution), because
  the stage1, the rlib and clang all have to be the same host's. `just test-bash`
  injects the result as `/bin/bash` — still not a `BOOT_BINS` entry, so an image
  build never depends on bash having been built — and drives
  `tools/smoke/bash.tsv` in the guest: six steps, all green (the banner, `-c`,
  arithmetic, a loop, a redirect read back through a second process, an external
  command, which is bash's fork+exec path, and `$PWD` from its own startup).
  CI's `bash` job runs that pair for each of the three arches and publishes the
  shell as the `bash-<arch>` artifact; the release workflow downloads it and
  injects the same `/bin/bash` into the image it publishes, so a released image
  carries a shell for scripts. The job is in the release's `needs`, so a C surface
  that builds bash but breaks it blocks a release.

  Both traps this route has are handled by the tools rather than by whoever runs
  them: the **forced relink** (`make` reports success *without* one when only the
  rlib changed — the rlib is a prerequisite of no bash target, so the binary's
  mtime is the only evidence) and the **rlib's two builders**
  (`tools/build-bash.py` deletes the stale rlib and rebuilds with this host's
  stage1, which is what makes `tools/cc-minix.py`'s newest-by-mtime pick the
  right one; otherwise the link fails `E0460: found possibly newer version of
  crate core`).

  **An interactive bash is covered too (2026-10-05).** The `bash -c` scenario
  cannot reach readline: it types into the minix shell and waits for its `#`
  prompt, which bash's `bash-5.3#` replaces. `just test-interactive-bash-<arch>`
  boots the split system image and drives a typed line at `bash-5.3#` through
  `tools/smoke/interactive_bash.py`, exercising readline's raw-mode setup and its
  poll-then-read path. That path *was* broken: the console's bytes sat in the
  kernel serial ring until a blocking `read`, so `do_select` never saw a typed byte
  and readline spun for ever; the tty server now pumps the ring on its poll path
  (`crates/servers/src/tty.rs`, `silent-failure-traps`). All three arches pass.
  (`tools/build-bash.py`, `tools/cc-minix.py`, `Justfile`,
  `tools/smoke/interactive_bash.py`, `crates/servers/src/tty.rs`, `.notes/c_build.md`)
- **~~`signal.h` and `minix-libc` disagree on `sigprocmask`~~ — FIXED
  (2026-09-24).** `minix-std`'s signature is now
  `sigprocmask(how, set_ptr, old_ptr)` (PM's `m2l1`/`m2l2`, both caller
  pointers), the libc export is the POSIX three-argument form, and
  `sigsuspend`/`sigpending` — which PM already implemented as
  `do_sigsuspend`/`do_sigpending` but nothing exposed — are now in `minix-std`,
  the libc and `signal.h`.
  (`tools/c-include/signal.h`, `crates/minix-libc/src/lib.rs`)
