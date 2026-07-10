# Fork/Exec/Exit/Waitpid Hang Analysis

## Summary

The hang at `# 123` is caused by an **endpoint format mismatch** between the PM server's process table and the kernel's process table after fork. The child's exit notification from the kernel is addressed with an endpoint format that PM cannot decode back to its own MProc slot, so the exit is silently absorbed, the child is never marked ZOMBIE, and the parent's `waitpid` blocks forever.

---

## Root Cause: `pm_isokendpt` slot extraction disagrees with real slot

### How fork creates the mismatch

When PM handles a fork (`handle_fork` in `crates/servers/src/pm.rs`):

1. **PM allocates a local slot** via `alloc_proc()` (line 1109 → `do_fork` line 547).  
   After the 11 boot processes (slots 0–10), the first child gets **PM slot 11**.  
   `do_fork` initially sets `mp_endpoint = child_slot | 0x8000` (line 568), i.e. `0x800B`.

2. **PM calls SYS_FORK kernel call** (`send_kernel_call(0, &kmsg)`, line 1125).  
   The kernel's `do_fork_handler` (`crates/kernel/src/system.rs:3746`) clones the Proc entry, then
   sets the child's endpoint with its own slot numbering:

   ```
   let child_slot = first free kernel Proc slot;   // e.g. 12
   (*rpc).p_endpoint = make_endpoint(1, child_slot);  // = (1 << 15) + 12 = 0x800C
   ```

   The kernel iterates `proc_addr(slot)` for slot in 0..256 (`NR_PROCS`). After the 17 boot
   processes (kernel tasks -5..-1 map to ProcTable[0..4], system processes 0..11 map to
   ProcTable[5..16]), the **first free kernel slot is 12** → endpoint `0x800C`.

3. **PM copies the kernel's endpoint** back into its MProc (line 1130–1134):

   ```rust
   let child_endpoint = unsafe { kmsg.m_payload.m1.m1i1 };
   (*child_ptr).mp_endpoint = child_endpoint;   // overwrites with 0x800C
   ```

   After this, `mp_endpoint == 0x800C` but the child lives in **PM slot 11**.

### Why the exit notification is lost

When the child calls `exit(1)`:

1. **Kernel** (`sys_exit_handler`, `syscall.rs:264`): stores `p_endpoint` (`0x800C`) in
   `PENDING_EXITS`, then calls `mini_notify` to PM.

2. **PM's notification handler** (`pm_server_main`, `pm.rs:1623` or `pm_dispatch`, `pm.rs:1377`):
   calls `SYS_GETKSIG`, gets endpoint `0x800C`, calls `pm_isokendpt(0x800C)`.

3. **`pm_isokendpt`** (`pm.rs:894`):

   ```rust
   pub unsafe fn pm_isokendpt(endpoint: i32) -> Option<usize> {
       let proc_nr = (endpoint & 0x7FFF) as usize;   // strips to 12
       ...
       let rmp = unsafe { &*base.add(proc_nr) };       // reads PM slot 12
       if rmp.mp_flags & IN_USE == 0 { return None; }  // slot 12 NOT in use!
   ```

   `0x800C & 0x7FFF = 12` — but **the child is in PM slot 11**. PM slot 12 is not in use
   (PM has only allocated slots 0–11). The function returns `None`.

4. **The exit is silently absorbed** — `do_exit` is never called, the MProc is never marked
   `ZOMBIE`, and the waitpid reply is never sent.

5. **Parent (shell) is blocked forever** on `waitpid()` → SENDREC to PM.

### Slot numbering summary

| Entity              | Slot allocator           | Slot value | Endpoint                    |
|---------------------|--------------------------|------------|-----------------------------|
| PM (MProc)          | `alloc_proc()` → PM slot | 11         | `mp_endpoint = 0x800C`     |
| Kernel (Proc)       | `proc_addr(slot)` scan → kernel slot | 12 | `p_endpoint = 0x800C` |
| `pm_isokendpt`      | `endpoint & 0x7FFF`      | **12**     | looks at PM slot 12 (wrong) |

**The core issue**: `pm_isokendpt` uses `endpoint & 0x7FFF` to extract a PM slot number, but
the kernel's endpoint encodes the **kernel's slot** number in those bits, not PM's slot number.
PM and kernel use independent slot allocators, so the numbers almost never match.

---

## Full Flow Walkthrough (with the bug)

### Phase 1: Fork

| Step | Process | Action | File:Line |
|------|---------|--------|-----------|
| 1 | Shell | `fork()` → SENDREC `PM_FORK` to PM | `minix-rt/src/lib.rs:496-508` |
| 2 | Kernel | Deliver SENDREC (PM in RECEIVE → direct delivery) | `ipc.rs:139-190` |
| 3 | PM | `handle_fork(shell_slot, msg)` | `pm.rs:1108` |
| 4 | PM | `do_fork(shell_slot)` → alloc PM slot 11, `mp_endpoint = 0x800B`, `mp_parent = shell_slot` | `pm.rs:536-577` |
| 5 | PM | SYS_FORK kernel call | `pm.rs:1117-1129` |
| 6 | Kernel | `do_fork_handler`: copies Proc, allocates kernel slot 12, `p_endpoint = 0x800C`, clears child's RECEIVING/SENDING flags → child is runnable | `system.rs:3746-3870` |
| 7 | PM | Reads child endpoint from kernel reply: `mp_endpoint = 0x800C` (overwrite) | `pm.rs:1130-1134` |
| 8 | PM | SEND VFS_PM_FORK to VFS | `pm.rs:1157-1163` |
| 9 | VFS | `service_pm()`: `pm_fork()`, SEND `VFS_PM_FORK_REPLY` to PM | `vfs/pm.rs:187-211` |
| 10 | PM | `handle_vfs_reply`: SEND reply (OK + child PID) to shell | `pm.rs:1517-1547` |
| 11 | Shell | `fork()` returns child PID (SENDREC unblocked by step 10) | `minix-rt/src/lib.rs:499-507` |

### Phase 2: Waitpid

| Step | Process | Action | File:Line |
|------|---------|--------|-----------|
| 12 | Shell | `waitpid(pid)` → SENDREC `PM_WAITPID` to PM | `minix-rt/src/lib.rs:514-525` |
| 13 | PM | `handle_waitpid`: `do_waitpid` → no zombie yet → `mp_wpid = pid` → EDONTREPLY | `pm.rs:1186-1207` |
| 14 | PM | Main loop: skips reply (EDONTREPLY), loops to RECEIVE(ANY) | `pm.rs:1674-1683` |
| 15 | Shell | Blocked on SENDREC (RECEIVING + REPLY_PEND) | — |

### Phase 3: Child exec → exit (bug triggers here)

| Step | Process | Action | File:Line |
|------|---------|--------|-----------|
| 16 | Child | `exec_replace("/bin/123")` → not in initramfs → returns -2 | `syscall.rs:927-1007` |
| 17 | Child | Prints "sh: 123: not found" | `userland/src/lib.rs:746-748` |
| 18 | Child | `exit(1)` → `sys_exit_handler` | `syscall.rs:264-294` |
| 19 | Kernel | Push `(endpoint=0x800C, status=1)` to PENDING_EXITS | `syscall.rs:279` |
| 20 | Kernel | `mini_notify(child_ep=0x800C, PM_PROC_NR=0)` → PM woken | `syscall.rs:282`, `ipc.rs:375-422` |
| 21 | Kernel | Set SLOT_FREE, dequeue child | `syscall.rs:285-291` |

### Phase 4: PM processes notification (the drop)

| Step | Process | Action | File:Line |
|------|---------|--------|-----------|
| 22 | PM | RECEIVE returns, `m_type == -10` → notification path | `pm.rs:1623` |
| 23 | PM | `SYS_GETKSIG` → returns `endpoint=0x800C, status=1` | `pm.rs:1625-1639` |
| 24 | PM | `pm_isokendpt(0x800C)` → **slot = 12, not in use → None!** | `pm.rs:894-911` |
| 25 | PM | **Exit silently dropped** — child never marked ZOMBIE | `pm.rs:1400` skip |
| 26 | PM | Loop continues, RECEIVE(ANY) → blocks | `pm.rs:1671` |
| 27 | Shell | **Hanging forever** on waitpid SENDREC | — |

---

## Why `pm_isokendpt` finds the wrong slot

```
pm_isokendpt(0x800C):
  proc_nr = 0x800C & 0x7FFF  = 12
                                ^^^^
  But the child's MProc is at PM slot 11.
  Slot 12 is empty → returns None.
```

The `& 0x7FFF` masking assumes the endpoint format is `(flags << 15) | pm_slot`, inherited from
MINIX 3 where PM and the kernel share slot numbers. In this Rust port, PM allocates slots
independently via `alloc_proc()`, while the kernel's `do_fork_handler` allocates slots from
its own Proc table. The slot numbers diverge.

---

## Fix Options

### Option A (recommended): Search by endpoint, not slot

Change `pm_isokendpt` to iterate all in-use MProc slots and match by `mp_endpoint` value
instead of computing the slot from endpoint bits:

```rust
pub unsafe fn pm_isokendpt(endpoint: i32) -> Option<usize> {
    if endpoint < 0 {
        return None;
    }
    let base = MPROC.as_ptr();
    for i in 0..NR_PROCS {
        let rmp = unsafe { &*base.add(i) };
        if rmp.mp_flags & IN_USE != 0 && rmp.mp_endpoint == endpoint {
            return Some(i);
        }
    }
    None
}
```

### Option B: Unify slot numbers

Make `do_fork` (PM) and `do_fork_handler` (kernel) use the same slot number. The kernel's
`do_fork_handler` already receives PM's child_slot hint via `msg[12..16]` (`FORK_SLOT_OFF`)
but ignores it and searches independently. Fixing this requires either:

- PM to pre-allocate the kernel slot and pass it, or
- The kernel to accept PM's slot hint and use it as the Proc slot number

This is more invasive and couples PM and kernel slot allocation.

---

## Other potential issues considered and ruled out

| Hypothesis | Verdict |
|------------|---------|
| `sys_exit_handler` frees the kernel Proc slot before notification arrives | **Not a problem** — `mini_notify` is synchronous and delivers directly because PM is in `RECEIVE(ANY)`. The notification is dispatched before SLOT_FREE is set. |
| SENDREC deadlock (shell → PM, child → PM) | **Ruled out** — `will_receive` correctly rejects processes with `SENDING` flag, and SENDREC's `mini_send` phase delivers directly when PM is in RECEIVE, so `SENDING` is never set on the shell. |
| Timer preemption not working | **Ruled out** — user says it works. |
| Page table / VM isolation issues | **Ruled out** — all processes share the same page table, so message buffers are readable. |
| Child not runnable after fork | **Ruled out** — `do_fork_handler` clears `RECEIVING`, `SENDING`, and other blocking flags on the child (line 3840-3852). Child's rts_flags becomes 0 (runnable). |
| VFS_PM_FORK_REPLY never arrives | **Ruled out** — both PM's SEND to VFS and VFS's SEND back are direct deliveries (both sides in RECEIVE), so the roundtrip completes immediately. |
| `handle_fork` sets VFS_CALL preventing reply | **Not a problem** — `handle_vfs_reply` clears VFS_CALL before replying. |
| Notification arrives before WAITPID sets mp_wpid | **Ruled out** — the timeline is sequential: fork reply → shell calls waitpid → PM sets mp_wpid → PM loops to RECEIVE → child exits → notification. |
