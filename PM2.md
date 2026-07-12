# Fork/Exec/Waitpid Hang Investigation

## Current State (2026-07-12)

**Fork works at the kernel level.** The child page table is created, the child
is enqueued, `restore()` switches to the child, and the child executes
user-mode code. Diagnostic output shows:
- `F` — `do_fork_handler` reached, child created with endpoint 0x800C (32780)
- `E0C80` — scheduler context-switches to the child
- The child makes progress (no longer stuck in the shell's read loop)

**Current blocker:** VFS crashes with a NULL pointer dereference when PM sends
`VFS_PM_FORK`. The page fault trace shows:
```
v=0e e=0004 i=0 cpl=3 IP=001b:000000000105cfff CR2=ffffffffffffff8b
```
- Vector 14 (Page Fault), error code 4 (user-mode read from non-present page)
- RIP=0x105cfff — within VFS's virtual address range (VFS loaded at 0x1000000)
- CR2=0xFFFFFFFFFFFFFF8B — non-canonical address, computed from NULL pointer
  minus offset 117 (i.e., `(NULL as *const u8).sub(117)`)

The child IS created and scheduled, but the fork protocol between PM→VFS fails
because VFS cannot handle the `VFS_PM_FORK` message.

## Bugs Found & Fixed This Session

### Bug 4: `save_proc_regs` called AFTER dispatch (FIXED)

**File:** `crates/kernel-boot/src/main.rs` in `syscall_handler_c()`
**C reference:** `SAVE_PROCESS_CTX` macro in `arch/i386/sconst.h` saves
registers at the START of the trap handler (assembly), before any dispatch.

**Bug:** Our code called `save_proc_regs()` AFTER `dispatch_basic_syscall()`.
For blocking syscalls like `SENDREC` (used by `fork()` to contact PM), the
dispatch function marks the process as blocked (`RECEIVING`) and returns
immediately. `mini_receive` at line 370-378 sets `RECEIVING` and dequeues the
process, then returns OK. The post-dispatch `save_proc_regs` was NEVER reached
because the code flow was:
```rust
let result = dispatch_basic_syscall(...);  // SENDREC: sets RECEIVING, returns OK
save_proc_regs(rp, saved);  // ← runs but captures POST-dispatch state
```

Wait — actually `save_proc_regs` WAS reached, because dispatch_basic_syscall
RETURNS after the blocking. So `save_proc_regs` was called, but it captured
`p_reg` with RAX = OK = 0 (from the result) instead of RAX = 48 (the SENDREC
syscall number). More importantly, for the PREVIOUS syscall (`read()`), the
`save_proc_regs` DID capture the correct state. But for the `fork()` SENDREC,
`save_proc_regs` ran AFTER dispatch, capturing the post-block state. The
correct fork return RIP WAS in RCX (saved[2] = RCX from `push rcx`), but then
the post-save ALSO ran, RE-capturing RCX (which should still be the fork
return address).

**The REAL issue:** `save_proc_regs` saved the registers saved by the push
sequence in `syscall_entry`. This includes RCX (RIP after syscall) from the
hardware's `syscall` instruction. The dispatch doesn't modify the saved
registers on the kernel stack (saved[0..13]). So `save_proc_regs` after
dispatch should still capture the correct RCX.

**The actual fix needed:** Move `save_proc_regs` BEFORE dispatch to match
the C code pattern. This is the correct approach and avoids any subtle issues
with dispatch modifying the stack save area. Also, the second (post-dispatch)
save was redundant and was removed.

**Fix:**
```rust
if nr != 61 {  // except SYS_EXEC_REPLACE which replaces the process image
    save_proc_regs(rp, saved);
}
let result = dispatch_basic_syscall(rp, nr, &args);
```

### Bug 5: `deliver_msg` set RAX to source endpoint instead of result code (FIXED)

**File:** `crates/kernel-boot/src/main.rs` in `deliver_msg()`
**C reference:** `delivermsg()` in `arch/i386/memory.c` line 502-503:
```c
if(!(rp->p_misc_flags & MF_CONTEXT_SET)) {
    rp->p_reg.retreg = r;  // r = copy result (0 = OK)
}
```

**Bug:** Our `deliver_msg` set RAX to the source endpoint read from the
message header (bytes 0-3). For PM's reply, this would be PM_PROC_NR = -3.
The `fork()` function in minix-rt checks `if reply < 0` — and -3 IS < 0,
causing `fork()` to return -3 as an error. The shell would then print
`sh: fork failed`.

The C code sets `retreg = r` where `r` is the return value of the message
copy operation (0 = OK). The source endpoint is in the message buffer
(`msg.m_source` at bytes 0-3), which the user code reads from the buffer,
not from RAX.

**Fix:** Set RAX to the result of `delivermsg()` (which returns OK = 0 on
success). Also added the missing `p_delivermsg.m_source = NONE` cleanup
after delivery (matching C delivermsg behavior).

```rust
let result = kernel::ipc::delivermsg(rp);
kernel::hal::write_retval(&mut (*rp).p_reg, result as u64);
// ... clear DELIVERMSG flag, set m_source = NONE ...
```

### Bug 6: `VM_PAGING_FORK` didn't set up kernel identity map in child's PML4 (FIXED)

**File:** `crates/kernel/src/system.rs` in `do_vm_paging_handler()` for
`VM_PAGING_FORK`

**Bug:** The handler copied kernel entries (256-511) from parent to child,
created fresh page tables for user pages via `map_page`, but never set up
PML4 entry 0 with the kernel identity map. The kernel code at `0x200000`
is in PML4 entry 0's range (0-512GB). After `restore()` switched CR3 to
the child's PML4, the next instruction fetch at `0x20xxxx` (inside
`restore` itself) would page-fault because entry 0 was not present.

**Fix approach 1 (PML4[0] overwrite — WORKS but dangerous):** After the
`map_page` loop, overwrite PML4[0] with the boot CR3's entry 0. This
preserves the kernel identity map but LOST user page mappings set up by
`map_page`. It works because the boot identity map (via 2MB huge pages)
still covers user pages at 0x1000000 and 0xFE00000. However, the stack
physical address might differ from the boot identity map's 2MB mapping,
causing the stack to point to the wrong physical memory.

**Fix approach 2 (PD entry copy — currently applied):** Keep PML4[0] as
created by `map_page` (with user mappings), but copy boot PD entries for
indices 1-7 (covering 0x200000-0x1000000, the kernel code range) into
the child's PD. This preserves user mappings AND provides the kernel
identity map.

**Boot CR3 switch:** Added a temporary switch to the boot CR3 to access
the parent's PML4 through the identity map. The boot CR3 identity-maps
the first 1GB using 2MB huge pages (from the trampoline setup). Without
this switch, the kernel runs with VM's CR3 which also has the identity
map (deep-copied from boot during `boot_create_restricted_page_table`).

### Bug 1: SCHED server `schedule_process` is a no-op (FIXED)

The SCHED server's `schedule_process()` at `crates/servers/src/sched.rs:360`
had a `// TODO` comment and never called `SYS_SCHEDULE`. When
`SCHEDULING_NO_QUANTUM` was handled, the child's `RTS_NO_QUANTUM` was
never cleared and it stayed dequeued forever.

**Fix:** Added `sys_schedule()` helper that calls `minix_rt::kernel_call(3, ...)`
to invoke the kernel's `do_schedule_handler`, which clears `RTS_NO_QUANTUM`
and re-enqueues the process.

**However:** Not triggered for boot processes — they're kernel-scheduled
(`p_scheduler` is null because `SYS_SCHEDCTL` is never called).

### Bug 2: SCHED server replies to NO_QUANTUM via SENDREC (FIXED)

The SCHED server's `sched_server_main` used `SENDREC_CALL` (syscall 48) to
reply to ALL message sources, including `SCHEDULING_NO_QUANTUM` where the
source is the process that lost quantum (the child). `SENDREC` sends AND
then RECEIVES — the RECEIVE part blocks the SCHED server forever.

**Fix:** Use `SENDNB_CALL` for `SCHEDULING_NO_QUANTUM` replies. Added
`SENDNB_CALL: u64 = 51` constant.

### Bug 3: Kernel `sched_proc` ignores quantum parameter (FIXED)

**File:** `crates/kernel/src/system.rs`
**C reference:** `sched_proc()` in `system.c` sets `p_cpu_time_left` from
quantum.

**Bug:** Only set `p_priority`, ignored quantum and cpu parameters.

**Fix:** Updated to accept `quantum: i32` and set `p_cpu_time_left` via
`ms_2_cpu_time(quantum)`. Updated `do_schedule_handler` to pass quantum.

## Current Blocker: VFS/PM IPC Hang on Exec

### What Happens

The timer ISR #GP is fixed. The shell now receives input ("abc") without
crashing, but the system silently hangs after the fork/exec path is
initiated. The flow:
1. Shell reads "abc" → fork() → PM creates child → VM creates page table
2. PM sends VFS_PM_FORK to VFS
3. System hangs — no "sh: abc: not found" or any error output

The kernel diagnostic `E0C80` (child scheduled) appears when the timer
is masked, confirming the child IS created and scheduled. The hang is
likely in the IPC exchange between PM, VFS, or the SCHED server during
exec.

### Possible Causes

1. **VFS process table not synced with PM:** VFS might try to look up the
   new child process by endpoint, but VFS's internal process table doesn't
   have the child yet. PM sends VFS_PM_FORK to notify VFS, but VFS's
   lookup function might not handle the "process not found" case correctly.

2. **Uninitialized pointer in VFS:** Some VFS data structure (like a file
   table, inode cache, or mount point) might be NULL because it wasn't
   properly initialized during boot.

3. **SCHED server handshake missing:** During fork, PM should send
   `SCHEDULING_START` to the SCHED server for the new child. If the
   SCHED server never receives this, the child might be stuck waiting
   for a scheduling reply.

4. **VFS mount_root incomplete:** If VFS's root mount didn't fully
   complete during boot, file operations during exec might hang.

5. **TTY echo deadlock:** If the shell's stdin read goes through VFS→TTY
   and TTY is blocked waiting for a buffer or grant, the hang could be
   in the TTY server.

## Remaining Issues

### `SYS_SCHEDCTL` Never Called

`SYS_SCHEDCTL` (kernel call 4) sets `p_scheduler`, which determines
whether a process is kernel-scheduled or user-scheduled. Without it,
all processes are kernel-scheduled, and the SCHED server's quantum
management is bypassed. This means `proc_no_time` renews the quantum
instead of notifying the SCHED server, and `PREEMPTED` is never checked
because `pick_proc()` is only called from `syscall_handler_c`, which
requires a syscall to be made.

**To fix:** Either implement `SYS_SCHEDCTL` in the kernel and call it
from RS during boot, or handle the kernel-scheduled path more
aggressively (e.g., set `PREEMPTED` in the timer interrupt handler
itself, not just in `proc_no_time`).

### Bug 7: Timer ISR iretq frame had corrupted CS (FIXED)

**File:** `crates/arch-x86_64/src/apic.rs` — `timer_isr_entry()`

**Symptom:** After the shell receives input and tries to fork, the timer
ISR's `iretq` consistently #GPs with error code `0x0010` (selector at
GDT index 2, RPL=0). The `#GP` crash trace shows:
```
G1000 ...
```

**Root cause analysis:** The error code `0x0010` was initially assumed
to be an SS selector issue (SS.RPL=0 vs CS.RPL=3), but patching SS in
the frame did not fix the crash. Writing a diagnostic showed SS=0x0013
(correct) in the frame, yet iretq still #GPs with error code 0x0010.

The error code `0x0010` at GDT index 2 corresponds to `GUDATA_SEL` —
a **data segment descriptor**. Loading a data segment descriptor into
CS is invalid and causes #GP. The actual frame had CS=0x0010 (data
segment) at the CS position, not CS=0x001B (user code segment).

**Why CS was corrupted:** QEMU's `sysretq` does not set the SS segment
selector, leaving it with the kernel value 0x0010. When the timer
interrupt fires during a period where segment registers hold stale
values (after `sysretq`), the interrupt frame captures CS=0x0010
instead of CS=0x001B.

**Fix:** Instead of patching SS in the frame, pop all 5 iretq frame
entries and rebuild them with **hardcoded correct segment selectors**:
- CS = `0x001B` (GUCODE_SEL | RPL=3)
- SS = `0x0013` (GUDATA_SEL | RPL=3)

This discards any stale CS/SS values from the original frame.

```asm
pop    rcx              ; RIP
pop    rax              ; CS (discarded)
pop    r11              ; RFLAGS
pop    r10              ; old_RSP
add    rsp, 8           ; skip old_SS
push   0x0013           ; SS = GUDATA_SEL | RPL=3
push   r10              ; old_RSP
push   r11              ; RFLAGS
push   0x001B           ; CS = GUCODE_SEL | RPL=3
push   rcx              ; RIP
iretq
```

**Verification:** The system boots, receives shell input `"abc"`, and
processes the command without any #GP. The fork/exec path no longer
crashes.

**Also fixed (housekeeping):** Added missing `#[unsafe(naked)]` to
`exception_page_fault_entry`, `exception_double_fault_entry`, and
`exception_gpf_entry` in `crates/arch-x86_64/src/asm.rs`.

### Files Changed This Session

| File | Change |
|------|--------|
| `crates/kernel-boot/src/main.rs` | Moved `save_proc_regs` before dispatch; fixed `deliver_msg` to set RAX to result code; removed P1/P2 debug markers |
| `crates/kernel/src/system.rs` | Rewrote `VM_PAGING_FORK` handler to include boot CR3 switch and kernel identity map copy; fixed `sched_proc` to accept quantum; fixed `do_schedule_handler` to pass quantum; added fork diagnostic |
| `crates/kernel/src/sched.rs` | Removed `C` diagnostic from `pick_proc` |
| `crates/servers/src/sched.rs` | Added `sys_schedule()` calling `kernel_call(3)`; fixed NO_QUANTUM reply to use SENDNB; added `SENDNB_CALL` constant |
| `crates/arch-x86_64/src/apic.rs` | Rewrote timer ISR's iretq to pop and rebuild frame with hardcoded CS=0x001B, SS=0x0013 |
| `crates/arch-x86_64/src/asm.rs` | Added `#[unsafe(naked)]` to exception handlers; added SS fix to page fault handler iretq |
