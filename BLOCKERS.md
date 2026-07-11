# Blockers

## Blocker 1: VFS `mount_root()` — Root Filesystem Never Mounted

### Symptom

VFS boots and enters its main loop (`get_work()` / `handle_work()`).
External commands that need file I/O (`/bin/echo`, `/bin/ls`, `cd /`) fail
because the root filesystem vnode is never set up — `mount_root()` returns
`null`, `fp_rdir` and `fp_cdir` for all processes remain null, and every
path resolution starts from nowhere.

### Context

The Rust VFS `sef_cb_init_fresh()` is a simplified version of the original C
init. The original C flow (`.refs/minix-3.3.0/minix/servers/vfs/main.c` and
`.refs/minix-3.3.0/minix/servers/pm/main.c`) has a multi-step boot protocol:

1. PM sends `VFS_PM_INIT` for each boot process (endpoint/pid/slot).
2. PM sends `NONE` sentinel and synchronises.
3. VFS subscribes to DS events.
4. VFS calls `init_dmap()` then `map_service()` from RS's rproctab.
5. VFS mounts PFS.
6. VFS starts a worker thread to call `mount_fs(DEV_IMGRD, ...)` which
   does `req_readsuper` to MFS.

The Rust version (`crates/servers/src/vfs/main.rs`) skips steps 1-5 and calls
`mount_root()` directly. `mount_root()` (crates/servers/src/vfs/mount.rs)
does `req_readsuper(vmp, "mfs", 0, 0, 1)` via `fs_sendrec(MFS_PROC_NR, msg)`.

This `req_readsuper` → MFS IPC path has never been tested end-to-end on real
QEMU hardware.

### Additional deficiencies found by cross-reference

| Issue | File | Detail |
|-------|------|--------|
| No `VFS_PM_INIT` handshake | `crates/servers/src/pm.rs` | PM never sends boot process endpoints to VFS. VFS fproc table has only PM's entry (slot 0). All other processes have `fp_endpoint = -1` — no `fp_rdir`/`fp_cdir` can be assigned. |
| PM uses hardcoded endpoints | `crates/servers/src/pm.rs` | PM iterates `[0..10]` instead of calling `sys_getimage` from the kernel. PIDs are `ep + 1` (wrong), no parent hierarchy, missing signal sets/timers. |
| Missing `init_vnodes/init_vmnts/init_filps` | `crates/servers/src/vfs/main.rs` | These are never called during VFS init. Subsystems start uninitialized. |
| Synchronous mount | `crates/servers/src/vfs/main.rs` | `mount_root()` blocks the VFS main loop. No worker thread — VFS can't process other IPC during mount. |

---

## Blocker 1b: IPC Subsystem Bugs

### Context

The IPC implementation in `crates/kernel/src/ipc.rs` had bugs found by
cross-referencing against the original MINIX C (`proc.c`).

| Bug | Location | Description | Status |
|-----|----------|-------------|--------|
| **A** | `ipc.rs:143` | `mini_send` assertion checks `DELIVERMSG` (bit 6) instead of `REPLY_PEND` (bit 0). A destination with `REPLY_PEND` set passes when it should fail — nested SENDREC corruption hazard. | Open |
| **B** | `ipc.rs:170-172` | `mini_send` clears `REPLY_PEND` on the **destination** during direct delivery. Original C never does this. If the destination is concurrently in its own SENDREC, this corrupts that SENDREC's state. | Open |
| **C** | `mini_receive` (~line 319) | `try_async` is never called. Pending asynchronous messages (from `senda` or interrupt) are never delivered to blocking receivers — the process blocks forever even though a message is available. | Open |
| **D** | `ipc_status_add_call/ipc_status_add_flags` | Reads `p_misc_flags` as `u32`, truncates to `u16`, modifies low bits, writes back as `u32`. Flags in bits 16-31 (`FLUSH_TLB=0x10000`, `SENDA_VM_MISS=0x20000`, `STEP=0x40000`) are zeroed on every call. | **Fixed** |

Bug D was fixed by aligning the IPC status helpers with C MINIX layout: call
type in low 6 bits of `p_misc_flags`, flags shifted to bits 16+. The old code
treated `p_misc_flags` as `u16`, truncating bits 16-31 on every status operation.

Additionally, `may_send_to` privilege checking is disabled (`ipc.rs:526-529`),
and all atomic operations use `Ordering::Relaxed`.

---

## Blocker 2: Fork/Exec of External Binaries

### Symptom

Typing `abc` at the shell prompt now produces `sh: abc: not found\r\n`, but
the system hangs before printing the next `# ` prompt. The child successfully
forks, exec fails (binary not found in initramfs), writes the error, and exits.
PM receives the exit notification via `mini_notify` and enters its
notification handler. The WAITPID reply to the parent (shell) is not sent.

### Current State

**What works:**
- Fork SENDREC completes — shell receives child PID from PM.
- Child kernel Proc is created via `do_fork_handler`.
- Child enqueued, scheduler picks it.
- `exec_initramfs_for_target` fails gracefully (returns -2).
- Child writes `sh: abc: not found` to serial via `write_err`.
- Child calls `exit(1)` → `sys_exit_handler` fires → `mini_notify(SYSTEM, PM)`.
- PM receives the notification and enters the SYS_GETKSIG loop.
- `do_getksig_handler` finds the child, returns its endpoint, clears SIGNALED.
- `do_exit` marks child as ZOMBIE.
- `syscall_handler_c` enqueues PM at tail and rotates on PREEMPTED.
- Boot processes have correct priorities: VM/RS=7, PM/VFS/MFS/INIT=0.
- Timer preemption works: `proc_no_time` sets `RTS_PREEMPTED`, quantum renewed.

**What doesn't work:**
- PM never sends the WAITPID reply to INIT.
- `handle_waitpid` never runs because INIT's WAITPID message is queued in
  PM's caller_q behind stale messages from other servers (RS notification
  flood, VM messages from boot).
- By the time PM reaches INIT's WAITPID, the child has already exited and
  `do_getksig_handler` already processed it. `do_waitpid` should find the
  ZOMBIE child, but PM never gets that far — the stale caller_q entries
  are processed first, and while draining them, new messages keep arriving.

### Fixes Applied

| Fix | File | Detail |
|-----|------|--------|
| PM notification handler | `pm.rs` | Exit processing moved from `pm_dispatch` (dead code) inline into main loop before `continue`. |
| VFS fork notification | `pm.rs` | Changed from `SENDREC`/`SEND` to `NOTIFY`. |
| Child rts_flags | `system.rs` | Mask clears `NO_QUANTUM`, `BOOTINHIBIT`, `PREEMPTED`, `SENDING`, `VMINHIBIT`. |
| Child priority | `system.rs` | Child inherits parent's priority instead of hardcoded 5. |
| Timer preemption | `sched.rs` | `proc_no_time` sets `RTS_PREEMPTED` for kernel-scheduled processes. |
| Boot priorities | `kernel-boot/src/main.rs` | VM/RS=7/200ms, others=0/50ms (matching C MINIX). |
| `p_pending: u128` | `proc.rs`, `system.rs` | Changed from `u32` to `u128` to match C MINIX `sigset_t` (16 bytes, `__uint32_t __bits[4]`). `do_getksig_handler` copies 16 bytes via `copy_from_slice` — `u32` caused a panic (source length 4 != dst length 16). The panic handler (`hlt_loop`) silently halted. |
| `SYS_ENDKSIG` after `SYS_GETKSIG` | `pm.rs` | PM called `get_work()` (kernel call 7) but never `end_work()` (kernel call 8). `SIG_PENDING` accumulated in rts_flags, making PM non-runnable. |
| `send_sig` notification guard | `system.rs` | `send_sig` always called `mini_notify(SYSTEM, target)` even when target already had `RTS_SIGNALED`. Added C's `!RTS_ISSET(rp, RTS_SIGNALED)` guard. Without this, every `cause_sig` call flooded PM with notifications. |
| Server `SEND` → `SENDNB` | `pm.rs`, `vm/mod.rs`, `rs.rs` | PM, VM, and RS all used blocking `SEND` (syscall 46) to reply. If destination wasn't receiving, the sender blocked with `RTS_SENDING` forever. Changed to `SENDNB` (syscall 51). |
| `SYS_IPC_SENDNB` kernel handler | `syscall.rs` | Added syscall 51 handler for non-blocking send, plus `SENDNB_CALL` constant in `minix-rt`. |
| `restore()` zeroes r12-r15 | `asm.rs` | Callee-saved regs zeroed after context switch. |
| syscall register clobbers | `minix-rt/src/lib.rs` | All `syscallN` inline asm declares `lateout` clobbers for rdi, rsi, rdx, r8, r9, r10, r12-r15. Missing clobbers let LLVM hold stale values across syscall. |
| IPC status layout | `ipc.rs` | `ipc_status_add_call`, `ipc_status_add_flags`, `ipc_status_clear` now match C layout: call in low 6 bits, flags at bits 16+. Bug D resolved. |

### Root Cause Chain

The hang after `not found` is caused by a cascade of three issues:

1. **RS notification flood (~36K msg/s):** RS used blocking `SEND_CALL` (46)
   to reply. When the destination wasn't receiving, RS blocked with
   `RTS_SENDING`. RS's main loop behavior then generated continuous
   `mini_notify(RS, PM)` calls — 275K+ notifications in ~11 seconds.
   Fixed by changing RS to `SENDNB_CALL`.

2. **`send_sig` always notified:** Every timer tick (via `cause_sig` →
   `send_sig` → `mini_notify`) flooded PM even when PM already had
   `RTS_SIGNALED`. Fixed with C's `!RTS_ISSET` guard.

3. **Stale caller_q entries delay WAITPID:** Even after those fixes, PM's
   caller_q accumulated ~236K stale entries from before the fixes. Each is
   a notification (m_type=-10) that PM's handler processes uselessly
   (SYS_GETKSIG → NONE → SYS_ENDKSIG → continue). PM must drain all
   stale entries before reaching INIT's WAITPID, which is queued behind them.

### What's Still Broken

After draining stale caller_q entries and reaching INIT's WAITPID,
`do_waitpid` should find the ZOMBIE child (set by `do_exit`) and reply.
This path is believed correct but hasn't been observed completing because
the drain takes ~24 seconds at ~9.7K iter/s and new messages may still
arrive during the drain.

---

## Blocker 3: VFS `req_getdents` — Grant Table / SAFECOPYTO Data Transfer

### Symptom

Calling `req_getdents(MFS, root_inode, ...)` from VFS after `mount_root()`
succeeds at the IPC level (MFS receives `REQ_GETDENTS`, reads directory
entries from the RAM disk, returns OK), but the returned buffer contains
all zeros — entries are never copied from MFS to VFS.

### Context

Two separate issues:

**Issue A: Grant table not registered with kernel.**
VFS's `vfs_grant_init()` was never called during init. The function exists
and calls `register_with_kernel()` via `kernel_call(34)`, but no code path
invoked it. Fixed by calling `vfs_grant_init()` before `mount_root()`.

**Issue B: `SAFECOPYTO` → `virtual_copy` writes zeros.**
Even with the grant table registered, when MFS calls `kernel_call(SAFECOPYTO)`
to copy directory entries from MFS's address space to VFS's grant buffer, the
kernel's `virtual_copy` function writes zeros to the destination.

Key files:
- `crates/servers/src/vfs/grant.rs` — grant table init and registration
- `crates/servers/src/vfs/request.rs` — `req_getdents` IPC
- `crates/kernel/src/system.rs` — `do_safecopy_to_handler`
- `crates/kernel-boot/src/main.rs` — `boot_create_restricted_page_table`

Boot test output shows:
```
VM: direct @ MFS= 48 8b 3c 24   ← CR3 switch + read works (reads MFS entry point)
VM: copy data: 00 00 00 00        ← virtual_copy writes zeros
```

A direct CR3 switch + read (switch to MFS's page table, read from MFS's
code VA, restore) works correctly. The bounce-buffer copy through the
kernel stack produces zeros.

**Page table audit:** `boot_create_restricted_page_table` on x86_64
deep-copies PD entries — they point to the same physical 2MB frames as
the boot PD, not newly allocated ones. PD[1] (kernel stack at
`0x200000`-`0x3FFFFF`) maps the same physical memory in every per-process
page table. The kernel stack IS accessible after a CR3 switch.

**Stale hypothesis closed.** The earlier "fresh PD frames" theory was
ruled out. Root cause of `virtual_copy` zeros remains unknown.

---

## Blocker 5: VM Server's `vm_init_boot` Reads Own BSS, Not Kernel Proc Table — RESOLVED

### Symptom

The VM server starts, calls `vm_init_boot()` in `crates/servers/src/vm/mod.rs`,
and creates zero vmproc entries. Every `brk()` IPC from any process returns
`EINVAL` because `vmproc_lookup()` finds no entry for the caller.

### Root Cause

The `kernel` crate is linked as a library into VM's userspace binary. When
`vm_init_boot()` calls `kernel::table::proc_addr(slot)`, it returns a pointer
into VM's **own BSS copy** of `PROC_TABLE_ALIGNED` — not the kernel's actual
process table. The BSS is zeroed, so every slot shows `p_rts_flags = 0` (not
SLOT_FREE) and `cr3 = 0`, causing all slots to be skipped.

### Fix Applied

Added `VM_PAGING_QUERY_PROC` subcommand (5) to the existing `do_vm_paging_handler`
(kernel call 62, SYS_VM_PAGING). The handler runs in kernel context with
BOOT_CR3, so it correctly reads the kernel's real `Proc` table. The handler
returns `(in_use, endpoint, cr3)` for a given slot number.

Rewrote `vm_init_boot()` to call `minix_rt::kernel_call(62, ...)` for each
slot instead of calling `proc_addr()` directly. This matches the MINIX 3.3.0
pattern where VM calls `sys_getkinfo()` to retrieve boot process info from
the kernel, rather than accessing kernel data structures directly.

**Files changed:**
- `crates/kernel/src/system.rs` — added `VM_PAGING_QUERY_PROC = 5` subcommand
- `crates/servers/src/vm/mod.rs` — `vm_init_boot()` now uses kernel call

**Tests:** 3 host tests in `system.rs` verifying the handler returns correct
data for in-use, free, and invalid slots.

---

## Blocker 4: MFS Buffer Pool Allocation & Initramfs Loading — RESOLVED

The system boots to a shell prompt. MFS buffer pool allocation succeeds.
All boot processes load, the RAM disk is mapped, and init runs.

Previous issues that were fixed:
- Physical memory allocator overlapped the kernel binary (free pool now
  starts at `__kernel_end`).
- `.minixfs` orphan section was not included in the linker script output,
  so `__kernel_end` was too early.
- Initramfs size limit raised from 10 MB to 256 MB.
- `opt-level=2` set in workspace `Cargo.toml` (avoids `__rust_alloc_zeroed`
  LLVM codegen issue at O3).

**Still missing from claimed fixes:**
- `BrkAllocator::GlobalAlloc` never received the explicit `alloc_zeroed`
  override. The current code relies entirely on `opt-level=2` to avoid the
  LLVM bug.

---

## Testing

### Build

```sh
just build
```

### Run

```sh
just run
```

With QEMU diagnostics (interrupts + CPU resets):
```sh
qemu-system-x86_64 -nographic -m 256M -no-reboot -d int,cpu_reset \
    -kernel target/trampoline.elf \
    -device loader,file=target/kernel.bin,addr=0x200000
```

### Boot test suite

```sh
just test-boot
```

Runs kernel-space assertions in `crates/kernel-boot/src/boot_test.rs` after
VFS mount_root, exits via `isa-debug-exit`.
