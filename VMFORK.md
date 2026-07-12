# VM Fork Implementation Plan

## Background

The current Rust fork implementation has the wrong call order compared to
original MINIX 3.3.0 C:

```
Current Rust flow:                  Original C flow:
  PM → SYS_FORK (kernel)              PM → VM_FORK (VM server, synchronously)
  PM → VM_FORK (VM server)            VM → SYS_FORK (kernel, inside do_fork)
  (child shares parent's CR3)         (child gets its own page table + COW)
```

The C flow ensures the child gets a new page table (with COW shared phys_blocks)
**before** the kernel creates the child's Proc entry. The Rust flow creates the
kernel Proc entry first (with the parent's CR3), then VM tries to add a page
table — too late. The child runs with the parent's identity-mapped physical
pages until it touches a write, at which point there IS no COW mechanism because
the Rust VM has no COW phys_block/phys_region infrastructure.

---

## Summary of the Correct C Flow

From `.refs/minix-3.3.0/minix/servers/pm/forkexit.c` (line 77):
```c
/* Memory part of the forking. */
if((s=vm_fork(rmp->mp_endpoint, next_child, &child_ep)) != OK) {
    return s;
}
/* PM may not fail fork after call to vm_fork(), as VM calls sys_fork(). */
```

`vm_fork()` sends `VM_FORK` to VM server via `_taskcall()` (synchronous IPC).
VM's `do_fork()` (`.refs/minix-3.3.0/minix/servers/vm/fork.c`) is the authoritative
implementation. Here is what it does, step by step:

```c
int do_fork(message *msg)
{
    // 1. Validate parent endpoint (msg->VMF_ENDPOINT) and child slot
    //    (msg->VMF_SLOTNO).

    // 2. Copy parent's vmproc to child. SAVE the child's existing pt_t
    //    (pre-allocated pt_dir/pt_dir_phys/pt_pt[] arrays).
    origpt = vmc->vm_pt;
    *vmc = *vmp;                          // Copy ALL state from parent
    vmc->vm_slot = childproc;             // Restore slot identity
    region_init(&vmc->vm_regions_avl);    // Init empty region AVL tree
    vmc->vm_endpoint = NONE;              // Not ready yet
    vmc->vm_pt = origpt;                  // Restore pre-allocated page dir

    // 3. Allocate and initialize a new page directory via pt_new().
    //    pt_new() allocates a physical page for the PML4, zeros it,
    //    and copies the kernel mappings (pt_mapkernel).
    if(pt_new(&vmc->vm_pt) != OK) return ENOMEM;

    // 4. Copy all virtual memory regions from parent to child with
    //    shared phys_blocks (COW setup). map_proc_copy calls
    //    map_copy_region for each region, which:
    //    a. Creates a new vir_region struct for the child
    //    b. For each phys_region in the parent's region, calls
    //       pb_reference() to link the SAME phys_block to the child
    //       (this increments pb->refcount)
    //    c. Very importantly, calls memtype->ev_reference() on each
    //       phys_region — for anonymous memory this is NULL/no-op,
    //       but the key effect is refcount > 1
    //    d. Then map_writept() writes the page table entries for BOTH
    //       parent and child. Since refcount > 1, the writable()
    //       callback returns false (anon_writable checks refcount == 1),
    //       so page table entries are mapped read-only.
    if(map_proc_copy(vmc, vmp) != OK) {
        pt_free(&vmc->vm_pt);
        return ENOMEM;
    }

    // 5. Inherit only VMF_INUSE flag.
    vmc->vm_flags &= VMF_INUSE;

    // 6. Fork ACLs.
    acl_fork(vmc);

    // 7. CALL THE KERNEL: sys_fork() sends SYS_FORK to the kernel.
    //    This is the kernel call that creates the child's kernel Proc entry.
    //    The kernel's do_fork() (system/do_fork.c):
    //    a. Copies parent's Proc struct to child slot
    //    b. Assigns new endpoint with incremented generation
    //    c. Clears CR3 (rpc->p_seg.p_cr3 = 0)
    //    d. Sets RTS_VMINHIBIT — child won't run until VM clears it
    //    e. Returns child endpoint and parent's message buffer virtual addr
    if((r=sys_fork(vmp->vm_endpoint, childproc,
            &vmc->vm_endpoint, PFF_VMINHIBIT, &msgaddr)) != OK) {
        panic("do_fork can't sys_fork: %d", r);
    }

    // 8. Bind the child's new page table — switch VM's own address space
    //    to the child's page table so we can access its memory directly.
    if((r=pt_bind(&vmc->vm_pt, vmc)) != OK)
        panic("fork can't pt_bind: %d", r);

    // 9. Handle memory for the parent's message buffer in both parent and
    //    child. This triggers page faults that get resolved via the COW
    //    path — each process gets a private copy of the message buffer page.
    vir = msgaddr;
    if (handle_memory_once(vmc, vir, sizeof(message), 1) != OK)
        panic("do_fork: handle_memory for child failed");
    vir = msgaddr;
    if (handle_memory_once(vmp, vir, sizeof(message), 1) != OK)
        panic("do_fork: handle_memory for parent failed");

    // 10. Set reply field: VMF_CHILD_ENDPOINT.
    msg->VMF_CHILD_ENDPOINT = vmc->vm_endpoint;

    return OK;
}
```

---

## What Needs to Change

### Architecture Overview

We need three layers:

```
┌──────────────────────────────────────────┐
│ Layer 3: Kernel syscall interface          │
│   SYS_FORK (kernel call 0) — clones Proc  │
│   SYS_VMCTL (kernel call 27) — CR3/VMCTL  │
├──────────────────────────────────────────┤
│ Layer 2: VM server (crates/servers/src/vm/)│
│   do_fork() — orchestrates the fork        │
│   pt_new_for_fork() — page table + COW     │
│   region + phys_block infra                │
├──────────────────────────────────────────┤
│ Layer 1: PM server (crates/servers/src/pm/)│
│   handle_fork() — simplified: just call     │
│   VM_FORK, copy mproc, notify VFS           │
└──────────────────────────────────────────┘
```

### Phase 1: Fix the PM→VM Call Order (PM side)

**File: `crates/servers/src/pm.rs`**

Change `handle_fork()` to match the C flow:

1. **Call VM_FORK BEFORE SYS_FORK.** Remove the SYS_FORK kernel call from PM.
   Instead, `handle_fork` should:
   a. Allocate child MProc slot
   b. Send VM_FORK to VM via IPC with the child slot number
   c. Receive child endpoint from VM's reply
   d. Copy parent's mproc to child, set child endpoint
   e. Notify VFS, respond to parent

2. **Remove the `send_kernel_call(0, ...)` call** (SYS_FORK) from PM entirely.
   This responsibility moves to VM.

3. **The VM_FORK message must include:**
   - Parent endpoint (PM fills from caller)
   - Child slot number (PM fills from alloc_proc)
   - PM must receive: child endpoint (filled by VM's reply)

### Phase 2: Implement Full do_fork in VM Server

**File: `crates/servers/src/vm/mod.rs`**

Rewrite `do_fork()` to match the C implementation exactly:

```rust
fn do_fork(msg: &mut Message) -> i32 {
    let parent_ep = msg.m_source;
    let child_slot = msg.m_payload.m1.m1i1;  // VMF_SLOTNO

    // 1. Validate endpoints
    let parent_vmp = vmproc_lookup(parent_ep)?;
    // child slot must be empty, in range

    // 2. Copy parent vmproc to child (save child's pre-allocated pt)
    // 3. pt_new() — allocate new PML4
    // 4. map_proc_copy() — copy regions with COW (shared phys_blocks)
    // 5. acl_fork()
    // 6. sys_fork() — SYS_FORK kernel call
    //    This requires minix_rt::kernel_call(SYS_FORK, ...)
    // 7. pt_bind() + handle_memory_once() for message buffer
    // 8. Reply with child endpoint
}
```

### Phase 2a: Add SYS_FORK kernel call to minix-rt

**File: `crates/minix-rt/src/lib.rs`**

The VM server needs the ability to call `SYS_FORK` (kernel call 0). Add a
wrapper:

```rust
pub fn sys_fork(
    parent_ep: i32,
    child_slot: i32,
    flags: u32,
) -> Result<(i32, u64), i32> {
    let mut msg = [0u8; 64];
    // msg[8..12] = parent_ep  (FORK_ENDPT_OFF)
    // msg[12..16] = child_slot (FORK_SLOT_OFF)
    // msg[16..20] = flags     (FORK_FLAGS_OFF)
    let r = kernel_call(0, &mut msg);  // SYS_FORK = 0
    if r != 0 { return Err(r); }
    let child_ep: i32 = ...;  // msg[8..12]
    let msgaddr: u64 = ...;   // msg[16..24]
    Ok((child_ep, msgaddr))
}
```

### Phase 2b: Add kernel call 27 (SYS_VMCTL) for CR3 switching

**File: `crates/kernel/src/system.rs` + `crates/minix-rt/src/lib.rs`**

The kernel must implement `do_vmctl_handler` (SYS_VMCTL, kernel call 27)
with subfunctions for setting a process's CR3 and clearing VMINHIBIT.

In C, VM calls `sys_vmctl()` after page table setup. The relevant subfunctions:

- `VMCTL_SET_PAGETABLE` — Set a process's CR3
- `VMCTL_BOOTINHIBIT_CLEAR` — Clear VMINHIBIT/RTS_BOOTINHIBIT
- `VMCTL_CLEAR_PAGEFAULT` — Clear pagefault state

Looking at `.refs/minix-3.3.0/minix/kernel/system/do_vmctl.c`:

```c
// SYS_VMCTL subcodes relevant for fork:
// - VMCTL_SET_PAGETABLE (set CR3 for a process)
// - VMCTL_BOOTINHIBIT_CLEAR (clear BOOTINHIBIT flag)
```

The VM calls `sys_vmctl(vmp->vm_endpoint, VMCTL_BOOTINHIBIT_CLEAR, 0)` at the
end of `exec_bootproc()` to clear `RTS_BOOTINHIBIT`. For fork, the key call is
`sys_fork()` which internally handles VMINHIBIT.

### Phase 3: Implement map_proc_copy with COW in VM Server

**File: `crates/servers/src/vm/region.rs` + new `phys_block.rs`**

The `pt_new_for_fork()` function in `crates/servers/src/vm/proc.rs` currently
does a **full deep copy** of every user page (allocates new physical frames
and copies data). This is incorrect for COW — it wastes memory and doesn't
match the C behavior.

Replace `pt_new_for_fork()` with a COW-based `map_proc_copy()`:

1. **Create a phys_block abstraction.** Each phys_block represents a physical
   4KB page and has a refcount. When a process writes to a page with refcount
   > 1, the page fault handler allocates a new page and copies the data
   (the `mem_cow()` function in `.refs/minix-3.3.0/minix/servers/vm/pb.c`).

2. **Create a vir_region / phys_region abstraction.** Each virtual memory region
   (vir_region) has an array of phys_regions (offset → phys_block mapping).
   During fork, `map_proc_copy` iterates the parent's regions and creates new
   vir_region structs for the child. Each phys_region in the child points to
   the SAME phys_block as the parent (via `pb_reference()`, which increments
   refcount).

3. **When writing page table entries at fork time**, check `anon_writable()`:
   if phys_block refcount > 1, mark the page table entry read-only (clear PG_RW).
   This ensures the first write by either parent or child triggers a page fault.

4. **Implement COW page fault handler.** When a write page fault occurs on a
   page with refcount > 1 (PRESENT + not writable):
   a. `mem_cow()` in `.refs/minix-3.3.0/minix/servers/vm/pb.c`:
      - Allocate a new physical page
      - `sys_abscopy(old_page, new_page, PAGE_SIZE)` — copy data
      - Create a new phys_block for the new page (refcount = 1)
      - Replace the faulting phys_region's pb with the new one
      - Write page table entry: RW + present
   b. Clear the page fault (`sys_vmctl(ep, VMCTL_CLEAR_PAGEFAULT, 0)`)

### Phase 3a: Page Fault IPC

When a page fault occurs in a user process, the kernel must forward it to the
VM server. Looking at the C flow:

1. Kernel's `page_fault_handler` in `arch/i386/do_exception.S` catches #PF
2. If the faulting process has VM set up, kernel sends `VM_PAGEFAULT` message
   to VM server (via notification-like IPC)
3. VM's `do_pagefaults()` handler resolves the fault:
   - Looks up the faulting address in the process's region tree
   - If COW: calls `map_pf()` → `anon_pagefault()` → `mem_cow()`
   - Clears page fault via `sys_vmctl(CLEAR_PAGEFAULT)`
4. Kernel re-enqueues the process, it retries the faulting instruction

The current Rust kernel already has a `SYS_VM_PAGING` handler (kernel call 62)
with subcommands. We need to add the VM_PAGEFAULT notification from kernel to VM.

### Phase 3b: add `SYS_VMCTL` subfunction for clearing pagefault

After handling a COW page fault, VM must tell the kernel to clear the pagefault
state and re-enqueue the process. This is `VMCTL_CLEAR_PAGEFAULT` subfunction
of `SYS_VMCTL` kernel call.

### Phase 4: Rewrite Kernel do_fork_handler for VM-Before-Kernel Order

**File: `crates/kernel/src/system.rs`**

The kernel's `do_fork_handler` currently:
- Copies parent Proc to child
- Sets child CR3 = parent's CR3 (WRONG — child should start with CR3=0)
- Clears all RTS flags so child is immediately runnable (WRONG — should set VMINHIBIT)

Change to match C (`do_fork.c`):

```rust
pub unsafe fn do_fork_handler(caller: *mut Proc, msg: &mut [u8; MESSAGE_SIZE]) -> i32 {
    // 1. Validate parent (check caller or msg parent endpoint)
    // 2. Get child slot from msg
    // 3. Assert child slot is empty (EMPTY check)
    // 4. Clone parent Proc to child
    // 5. Assign new endpoint (increment generation)
    // 6. Set child's retreg = 0 (so child sees fork return value 0)
    // 7. Clear timer/profiling misc flags
    // 8. Set child's CR3 = 0 (rpc->p_seg.p_cr3 = 0)
       //   In C: rpc->p_seg.p_cr3 = 0; rpc->p_seg.p_cr3_v = NULL;
    // 9. Set RTS_VMINHIBIT — child doesn't run until VM clears it
    // 10. Set RTS_NO_QUANTUM — child needs scheduling
    // 11. Clear SIGNALED, SIG_PENDING, P_STOP from child
    // 12. Clear p_pending (sigset)
    // 13. Return child endpoint + parent's msgaddr
    // 14. DO NOT enqueue child — VMINHIBIT prevents running
}
```

### Phase 5: Shared-Physical-Stack Bug (RESOLVED)

The `cmd_path[200..]` workaround was caused by parent and child sharing all
physical pages before COW was implemented. With COW working:

- Parent's stack and child's stack start as the same physical pages (read-only)
- On first write by either, a COW page fault fires
- VM allocates a new page, copies data, maps it RW for the writer
- Each process gets its own private copy of modified pages

The workaround at `cmd_path[200..]` has been removed. The shell child now
derives the command name directly from `cmd_path` (a stack array preserved
via COW) by scanning for the last `/` before the null terminator.

### Phase 6: Supporting Infrastructure

#### 6a: Page Fault Delivery (Kernel → VM)

The kernel needs to catch user-mode write page faults (#PF) and forward them
to VM. Currently page faults with `PFERR_WRITE | PFERR_PROT` on a PRESENT page
need to be sent to VM.

Steps:
1. In the #PF handler, check if fault is a COW candidate (write to a
   read-only PRESENT page in user space)
2. If yes, craft a `VM_PAGEFAULT` message and send it to VM via kernel call
   or notification
3. VM processes it and replies with `VMCTL_CLEAR_PAGEFAULT`

#### 6b: Syscall Interface for VM to Update Kernel

VM needs two kernel calls after setting up the child:
- `SYS_VMCTL(VMCTL_CLEAR_PAGEFAULT, ep)` — after handling a page fault
- `SYS_VMCTL(VMCTL_BOOTINHIBIT_CLEAR, ep)` — to clear VMINHIBIT and make
  the child runnable (called from VM's do_fork after pt_bind + handle_memory)

Actually in the C flow, VM does NOT explicitly clear VMINHIBIT during fork.
The kernel's do_fork sets VMINHIBIT, and then VM just ensures the child's
page table is ready. The VMINHIBIT is cleared by... let me check.

Looking at the kernel do_fork.c:
```c
if(m_ptr->m_lsys_krn_sys_fork.flags & PFF_VMINHIBIT) {
    RTS_SET(rpc, RTS_VMINHIBIT);
}
```

And VM's do_fork calls `sys_fork()` with PFF_VMINHIBIT set. So VMINHIBIT is
set by the kernel's do_fork. Then who clears it?

Looking at pt_bind and the fact that VM calls `handle_memory_once` after
pt_bind — that triggers page faults in the child's address space that VM
resolves. After that, the child's page table is active. The VMINHIBIT must
be cleared by VM explicitly.

Actually, I think the flow is:
1. Kernel do_fork sets VMINHIBIT on child
2. VM's pt_bind activates child's page table (but VM is still running in its
   own context — pt_bind is a local VM operation that stores the CR3, not a
   kernel call)
3. VM's handle_memory_once triggers page faults → VM resolves them
4. VM then sends the reply to PM (which was waiting for VM_FORK response)
5. PM then eventually... the child gets scheduled when VMINHIBIT is cleared

But when is VMINHIBIT cleared? Let me look at how this works in the C code...

Actually, I think `pt_bind()` in the C code (which is `sys_vmctl(SET_PAGETABLE)`)
might also clear VMINHIBIT. Let me check the pagetable.c pt_bind function:

```c
int pt_bind(pt_t *pt, struct vmproc *who)
```

This is in `arch/i386/pagetable.c` or similar. It's architecture-specific and
might call `sys_vmctl()` to update the kernel's CR3 and clear VMINHIBIT.

Looking at the do_fork in fork.c more carefully:
```c
if((r=pt_bind(&vmc->vm_pt, vmc)) != OK)
    panic("fork can't pt_bind: %d", r);
```

The `pt_bind` in arch code calls `sys_vmctl(who->vm_endpoint, VMCTL_SET_PAGETABLE, ...)` 
which updates the kernel's p_cr3 and clears VMINHIBIT.

So the flow is:
1. VM calls `pt_bind(&vmc->vm_pt, vmc)` which calls `sys_vmctl()` kernel call
   with `VMCTL_SET_PAGETABLE` — this sets the process's CR3 and clears VMINHIBIT
   (making the child runnable)
2. Then `handle_memory_once()` resolves any pending page faults for the
   child's message buffer

So yes, `pt_bind()` in the C code (the architecture-specific one) calls
`sys_vmctl` with `VMCTL_SET_PAGETABLE` which:
1. Sets `proc->p_seg.p_cr3 = phys_cr3`
2. Clears `RTS_VMINHIBIT`
3. If the process is now runnable, enqueues it

**Rust implementation:** VM's `do_fork()` calls
`minix_rt::sys_vmctl_set_addspace(child_ep, child_cr3)` which invokes the
kernel's `do_vmctl_handler(VMCTL_SETADDRSPACE)` — this sets `p_seg.p_cr3`,
clears `RTS_VMINHIBIT`, and enqueues the child if runnable. The
VM-local `pt_bind()` (in `proc.rs`) is a separate helper that only stores
the CR3 in the Vmproc table and updates the kernel's Proc struct locally.

---

## Implementation Status

### Step 1: Add SYS_VMCTL kernel call with basic subfunctions ✅ DONE
- `do_vmctl_handler` exists in `crates/kernel/src/system.rs` with subfunctions:
  - `VMCTL_SETADDRSPACE` (29): sets p_cr3, clears VMINHIBIT, enqueues if runnable
  - `VMCTL_CLEAR_PAGEFAULT` (12): clears pending pagefault state
  - `VMCTL_GET_PDBR`, `VMCTL_FLUSHTLB`, `VMCTL_BOOTINHIBIT_CLEAR`, etc.
- Wrappers in `crates/minix-rt/src/lib.rs`: `sys_vmctl_set_addspace()`
- **Files:** `system.rs`, `minix-rt/src/lib.rs`

### Step 2: Add VM_PAGEFAULT notification from kernel to VM ✅ DONE

**What exists:**
- `handle_page_fault()` in `crates/kernel/src/vm.rs` catches user-mode #PF,
  stores fault info in `PAGE_FAULT_INFO`, sets `RTS_PAGEFAULT`, and calls
  `mini_notify(SYSTEM, VM_PROC_NR)` to notify VM via notification.
- VM's `sef_signal_handler()` iterates active Vmproc entries and calls
  `minix_rt::sys_vmctl_memreq_get(ep)` to read fault info from the **kernel's**
  copy via SYS_VMCTL kernel call, avoiding the static-data-duplication issue.

**Bug 1: NOTIFY_MESSAGE constant mismatch (FIXED)**
- Kernel's `build_notify_message()` hardcoded `m_type = -10` instead of
  `arch_common::com::NOTIFY_MESSAGE` (0x1000 = 4096).
- VM's `dispatch_message()` checked `msg.m_type == 0x1000` — never matched
  the kernel's `-10`. VM never detected notifications.
- All other servers (PM, RS, DS, TTY, VFS, sched) also checked for `-10`,
  creating an internally consistent but C-incompatible convention.
- **Fix:** Changed kernel to send `NOTIFY_MESSAGE` (0x1000), and updated ALL
  servers to check for `NOTIFY_MESSAGE as i32`.
- **Files:** `ipc.rs:build_notify_message`, `pm.rs` (2x), `rs.rs`, `ds.rs`,
  `vfs/main.rs`, `sched.rs`, `tty.rs`, `vm/mod.rs`

**Bug 2: pf_info_read uses VM's own static data copy (FIXED)**
- `handle_page_fault()` writes fault info to the KERNEL's `PAGE_FAULT_INFO`.
- VM's old `sef_signal_handler()` called `kernel::vm::pf_info_read()` which reads
  from VM's OWN copy of `PAGE_FAULT_INFO` (same Blocker 5 class — kernel
  crate static data is duplicated in userspace binaries).
  VM always read empty fault info → never processed page faults.
- **Fix:** Rewrote `sef_signal_handler()` to iterate the VM's own Vmproc table
  and call `minix_rt::sys_vmctl_memreq_get(ep)` (kernel call 43 with
  `VMCTL_MEMREQ_GET`) for each active endpoint. The kernel's `do_vmctl_handler`
  reads from its own `PAGE_FAULT_INFO` and returns the data.
- Added `for_each_active_vmproc()` iterator in `crates/servers/src/vm/proc.rs`.
- Also fixed `mem::sys_vmctl(VMCTL_CLEAR_PAGEFAULT)` to forward to the kernel
  via `minix_rt::sys_vmctl_clear_pagefault(ep)` instead of directly manipulating
  the kernel's Proc struct through VM's duplicated data.
- **Files:** `mod.rs` (sef_signal_handler), `proc.rs` (for_each_active_vmproc),
  `minix-rt/src/lib.rs` (sys_vmctl_memreq_get, sys_vmctl_clear_pagefault),
  `mem.rs` (VMCTL_CLEAR_PAGEFAULT kernel call forwarding)

### Step 3: (Re)implement phys_block/phys_region module ✅ DONE
- `crates/servers/src/vm/pb.rs` — `PhysBlock` (refcounted physical page)
  with `PhysBlockTable` (1024 entries), global `pb_new()`, `pb_ref()`,
  `pb_unref()`, `pb_get()`, `pb_find()`, and host unit tests.
- Note: `pb_unref()` calls `crate::vm::vm_free_pages()` which uses a
  kernel call (fix for Blocker 5 class — was `kernel::vm::free_mem`).

### Step 4: Implement COW page fault handler in VM ✅ DONE
- `crates/servers/src/vm/cow.rs` — `handle_cow_fault()` function.
  Walks the page table, finds the PhysBlock, and either marks writable
  (if last reference) or allocates a new page, copies data, and remaps
  as writable with a new PhysBlock (if refcount > 1).
- `mod.rs` — `handle_pagefault_for()` dispatches to cow handler on
  write-protection faults in writable regions.
- **Blocker 5 fix:** Uses `crate::vm::vm_alloc_pages(1)` and
  `crate::vm::vm_free_pages(new_phys, 1)` (kernel call 62 wrappers)
  instead of `kernel::vm::alloc_mem`/`free_mem` which access duplicated
  static data.

### Step 5: Rewrite pt_new_for_fork → map_proc_copy ✅ DONE
- `pt_new_for_fork()` in `crates/servers/src/vm/proc.rs` rewritten to:
  - Allocate child PML4 via `vm_alloc_pages(1)` (kernel call, not direct)
  - Walk parent's full page table hierarchy (PML4 → PDPT → PD → PT)
  - Register shared pages in PhysBlock table via `pb_find()`/`pb_new()` + `pb_ref()`
  - Map child pages read-only (COW setup)
  - **Make parent's pages read-only too** via `make_pte_readonly(parent_cr3, va)`
    when `refcount > 1`, matching C's `map_writept(src)` after `map_proc_copy`
  - PhysBlock refcount correctly accounts for both parent and child mappings
    (starts at 2 for shared pages via extra `pb_ref()` after `pb_new()`)
- `vm_clone()` now copies regions from parent to child.
- VM's `do_fork()` adds:
  - `vm_flags &= VMF_INUSE` flag cleanup (matching C `fork.c:84`)
  - `acl_fork()` call after clone (matching C `fork.c:87`)
  - `handle_memory_once()` for message buffer COW (matching C `fork.c:98-108`)
    calls `cow::handle_cow_fault()` for both child and parent on `msgaddr`
- `Vmproc` struct extended with `vm_acl: i32` field for ACL tracking.
- New constants: `USER_ACL = 0`, `NO_ACL = -1`, `acl_fork()` function.
- **Not done:** Region-level abstraction (VirRegion/PhysRegion) — the
  current implementation walks the raw page table rather than regions.

### Step 6: Rewrite PM's handle_fork to match C order ✅ DONE
- PM sends `VM_FORK` to VM via SENDREC
- VM calls `sys_fork()` inside its `do_fork()`, receives child endpoint
- PM receives child endpoint from VM reply, copies mproc, notifies VFS
- PM checks VM reply's `m_type` for errors (not just SENDREC return value)

### Step 7: Fix kernel's do_fork_handler ✅ DONE
- Child CR3 set to 0 (not parent's CR3)
- `VMINHIBIT` set on child — child doesn't run until VM clears it
- Child NOT enqueued — VM enqueues via `VMCTL_SETADDRSPACE`
- `NO_QUANTUM` cleared from child (inherited from parent, not in C's
  clear_rts list — C explicitly sets it, port doesn't have SCHED)
- `p_defer_r1 = 1` on child for `IS_FORK_CHILD` detection
- Kernel slot alignment fixed: PM reserves slot 11 (RAMDISK) so `alloc_proc`
  returns slot 12+, matching kernel's first free `proc_addr()`

### Step 8: Remove cmd_path[200..] workaround ✅ DONE
- Workaround removed from `crates/userland/src/lib.rs`.
- Shell child now derives the command name directly from `cmd_path`
  (a stack array preserved via COW) by scanning for the last `/`
  before the null terminator, rather than relying on register-based
  variables or a saved copy at offset 200.
- A local `cmd_name` array holds the extracted name so it survives
  subsequent modifications to `cmd_path` (e.g., `/sbin/` fallback).

## Additional Bugs Found & Fixed

### Bug A: mini_notify src_id for kernel tasks (FIXED)
- `mini_notify(SYSTEM, dst)` used `src_e as usize` for SYSTEM (-2),
  which overflowed the 64-bit notification bitmap.
- Added `priv_find_proc_id()` to search privilege table by `s_proc_nr`.

### Bug B: VM physical memory allocation (FIXED)
- `kernel::vm::alloc_mem`/`free_mem` has static data duplicated in VM's
  userspace binary (same Blocker 5 class). The allocator bitmap was never
  populated in VM's copy.
- Added `vm_alloc_pages(count)` and `vm_free_pages(pa, count)` wrappers
  in `crates/servers/src/vm/mod.rs` that use kernel call 62
  (`VM_PAGING_ALLOC`/`VM_PAGING_FREE`) instead of calling the kernel
  crate functions directly.
- Updated `pb.rs`, `proc.rs`, **and `cow.rs`** to use the kernel call wrappers.

### Bug C: NOTIFY_MESSAGE constant mismatch (FIXED)
- See Step 2 Bug 1 above.

### Bug D: COW handler used direct kernel-crate allocator (FIXED)
- `handle_cow_fault()` in `crates/servers/src/vm/cow.rs` was calling
  `kernel::vm::alloc_mem`/`free_mem` directly — same Blocker 5 class
  as Bug B. The kernel allocator bitmap was never populated in VM's
  copy, so COW page faults would always fail with `NO_MEM`.
- **Fix:** Replaced with `crate::vm::vm_alloc_pages(1)` and
  `crate::vm::vm_free_pages(new_phys, 1)` kernel call wrappers.

### Bug E: Parent retained RW after fork (FIXED)
- `pt_new_for_fork()` only made the child's PTEs read-only. The parent
  kept RW access to shared physical pages, meaning parent writes after
  fork would modify pages the child still read through its read-only
  mapping, violating fork semantics.
- C code calls `map_writept(src)` on BOTH parent and child after
  `map_proc_copy`. Both get read-only; the first write by either
  triggers COW.
- **Fix:** After setting up child PTEs and PhysBlock refcounting,
  walk parent PTEs and clear PG_RW on any page where PhysBlock
  refcount > 1 via `make_pte_readonly(parent_cr3, va)`.
  PhysBlock refcount starts at 2 for shared pages (extra `pb_ref()`
  after `pb_new()` to account for parent's existing mapping).

---

## Message Format for VM_FORK

The Rust VM_FORK IPC must match the C message format:

```rust
// C: VMF_ENDPOINT = m1.m1i1  (parent endpoint, offset 8)
// C: VMF_SLOTNO   = m1.m1i2  (child slot number, offset 12)
// C: VMF_CHILD_ENDPOINT = m1.m1i1  (reply, offset 8)

// Rust: Message.m_payload.m1 layout:
//   m1i1 at bytes 8..12  = parent endpoint (input) / child endpoint (reply)
//   m1i2 at bytes 12..16 = child slot number (input)
```

PM sends:
```rust
let mut msg = Message { m_source: 0, m_type: VM_FORK, m_payload: zeroed() };
msg.m_payload.m1.m1i1 = parent_endpoint;
msg.m_payload.m1.m1i2 = child_slot as i32;
// Send to VM_PROC_NR via SENDREC (synchronous)
```

VM replies:
```rust
msg.m_payload.m1.m1i1 = child_endpoint;  // set by kernel in sys_fork reply
msg.m_type = OK;                          // or error
```

---

## Key C Source Files Reference

| Component | C Source |
|-----------|----------|
| PM fork entry point | `.refs/minix-3.3.0/minix/servers/pm/forkexit.c` — `do_fork()` (line 44) |
| VM fork handler | `.refs/minix-3.3.0/minix/servers/vm/fork.c` — `do_fork()` (line 33) |
| Kernel fork handler | `.refs/minix-3.3.0/minix/kernel/system/do_fork.c` — `do_fork()` (line 26) |
| Region copy (COW setup) | `.refs/minix-3.3.0/minix/servers/vm/region.c` — `map_proc_copy()` + `map_copy_region()` |
| PhysBlock/refcount | `.refs/minix-3.3.0/minix/servers/vm/pb.c` — `pb_reference()` + `mem_cow()` |
| Page table alloc | `.refs/minix-3.3.0/minix/servers/vm/pagetable.c` — `pt_new()`, `pt_bind()`, `pt_free()` |
| Kernel VM control | `.refs/minix-3.3.0/minix/kernel/system/do_vmctl.c` |
| Anonymous page fault | `.refs/minix-3.3.0/minix/servers/vm/mem_anon.c` — `anon_pagefault()` (COW via `mem_cow()`) |
| Writable check | `.refs/minix-3.3.0/minix/servers/vm/mem_anon.c` — `anon_writable()` (refcount == 1) |
| Library helpers | `.refs/minix-3.3.0/minix/lib/libsys/vm_fork.c` — `vm_fork()` PM→VM call |
| Library helpers | `.refs/minix-3.3.0/minix/lib/libsys/sys_fork.c` — `sys_fork()` VM→kernel call |
