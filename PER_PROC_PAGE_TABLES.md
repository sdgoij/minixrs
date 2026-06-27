# Per-Process Page Tables: Current State & Fix Plan

## Overview

The Minix Rust port aims to give each process its own address space with
private physical copies of code and stack pages, preventing one process
from reading or writing another's memory. This document analyzes the gap
between the implementation and execution, and provides a step-by-step fix
plan.

---

## Current Implementation

### What exists

1. **`exec_setup_new_page_table()`** (`crates/arch-x86_64/src/arch_syscall.rs`):
   - Called from `asm_exec_handler` after `load_elf` and `setup_user_stack`
   - Creates a fresh per-process page table for the exec'd process:
     - Allocates PML4, PDP, PD (4KB each, zeroed)
     - Deep-copies all 512 BOOT_PD entries (kernel identity map for 0–1GB)
     - PML4[0] → new PDP → new PD (private identity map)
     - PML4[L4_SLOT_DIRECT] → BOOT_PDP (shared kernel high mapping)
     - PDP[3] → BOOT_PD3 (shared APIC MMIO mapping)
   - Splits 2MB PDEs at code and stack virtual ranges into 4KB page tables
   - Allocates new physical frames for each 4KB page in those ranges
   - Copies data from identity-mapped originals to private frames
   - Remaps each virtual page to its private frame in the new page table
   - Writes `p_cr3` and `p_cr3_v` on the exec'd process's `Proc` struct

2. **Exec target CR3 restore** (`crates/arch-x86_64/src/syscall.S`):
   - After `syscall_exec_check` returns non-zero (exec target), loads
     `CURRENT_PROC → p_cr3` (offset 0x80) and switches CR3
   - Happens BEFORE switching RSP to user space (kernel stack stays
     accessible if an interrupt fires)
   - The exec'd process starts running in its private address space

3. **`syscall_entry`** (same file):
   - On entry: loads `BOOT_CR3` to ensure kernel BSS is accessible
   - Does NOT save the incoming per-process CR3

4. **Normal return path** (label `1:` in `syscall.S`):
   - Restores user RIP/RFLAGS/RSP, `swapgs`, `sysretq`
   - Does NOT restore per-process CR3

### What's broken / incomplete

**Phase 1 (save/restore CR3 on every syscall) is implemented.**
The syscall entry saves the incoming per-process CR3, and the normal
return path restores it. Combined with Phase 4 below, the exec'd process
maintains its private address space across all syscalls.

**Consequences (all resolved):**

| Issue | Status |
|-------|--------|
| Isolation lost after first syscall | **FIXED** — CR3 saved at entry, restored on normal return |
| `load_elf` overwrites init | **FIXED** — private copies in per-process PT; Phase 4 writes IPC messages under target's CR3 |
| Exec'd process's data stale after syscall | **FIXED** — per-process CR3 restored on every return |

---

## Key Insight: The Two-Phase Approach

### Phase 1: Save/restore per-process CR3 on EVERY syscall entry/exit

For the exec'd process, every syscall must:
1. **Save** incoming CR3 at entry (before loading BOOT_CR3)
2. **Restore** it on return (after handler completes, before `swapgs`+`sysretq`)

Init never needs this: it runs on BOOT_CR3 (saves and restores BOOT_CR3,
a no-op). Only processes that have been given per-process page tables
(by `exec_setup_new_page_table`) benefit from Phase 1.

### Phase 2: Private-copy exec ✅ (DONE)

The approach implemented in `exec_setup_new_page_table`:

1. `load_elf` writes the new binary through BOOT_CR3 to identity-mapped
   pages (overwriting the originals)
2. `exec_setup_new_page_table` creates NEW private copies of the binary
   and stack pages in a per-process page table
3. On exec target return (`syscall.S`), CR3 switches to the per-process
   page table (p_cr3 from Proc struct)
4. The exec'd process now runs in its private address space

### Phase 4: Kernel writes to user memory under per-process CR3 ✅ (DONE)

`delivermsg()` in `ipc.rs` temporarily switches to the target process's
CR3 before writing IPC messages to its user buffer, then switches back
to BOOT_CR3. This ensures the message lands in the target's private
frames (not the identity-mapped originals).

If `p_cr3` is zero (process has no per-process page table, e.g. init),
the CR3 switch is skipped entirely — the write goes through BOOT_CR3,
which is correct for identity-mapped processes.

---

## Step-by-Step Fix Plan

### Phase 1: Save/restore per-process CR3 on syscall boundary ✅ (DONE)

**Goal:** After EVERY syscall, restore the process's per-process CR3 so
it runs in its own address space.

#### Step 1.1: Save incoming CR3 at entry

In `syscall_entry`, the incoming CR3 is saved before loading BOOT_CR3:

```asm
syscall_entry:
    swapgs
    mov     gs:0x8, rsp
    mov     rsp, gs:0x0
    push    gs:0x8
    push    r11
    push    rcx
    mov     rax, cr3
    push    rax                    ; save incoming per-process CR3
    ; Switch to boot CR3
    lea     rdx, [rip + BOOT_CR3]
    ...
```

**Stack layout (current):**
```
        +-----------------+
rsp →   | saved CR3       |  incoming per-process CR3
        | saved rcx       |  user RIP
        | saved r11       |  user RFLAGS
        | saved user RSP  |  stack for sysretq
        +-----------------+
```

This shifts all subsequent stack offsets by +8 compared to before Phase 1.

#### Step 1.2: Restore per-process CR3 on normal return

In the normal return path (label `1:`), saved CR3 is restored:

```asm
1:  ; Normal return
    mov     rax, [rsp + 0]      ; saved per-process CR3
    mov     cr3, rax            ; restore BEFORE switching to user RSP
    mov     rcx, [rsp + 8]      ; user RIP
    mov     r11, [rsp + 16]     ; user RFLAGS
    mov     rsp, [rsp + 24]     ; user RSP (last access in BOOT_CR3)
2:
    swapgs
    sysretq
```

CR3 is restored while the kernel stack is still active, so interrupts
have a valid stack.

#### Step 1.3: FORK_PARENT_RSP offset fixed

The FORK code reads `[rsp + 24]` for user RSP (changed from `[rsp + 16]`
to account for the pushed CR3).

#### Step 1.4: Note on init

Init always enters with BOOT_CR3 active, so the saved CR3 is BOOT_CR3
and the restore is a no-op. ✓

#### Step 1.5: IPC / syscall handlers

Handlers run on BOOT_CR3 after entry. User memory reads go through the
identity map — correct because `load_elf` writes the binary there.
User memory writes (IPC message delivery) go through the target's
per-process CR3 via Phase 4 (see below). ✓

### Phase 2: Exec with proper private copies ✅ (DONE)

**Phase 2 is fully implemented and active.**

| Step | File | Status | Details |
|------|------|--------|---------|
| 2.1 | `arch_syscall.rs` | **DONE** | `exec_setup_new_page_table` creates per-process PT with private code/stack copies |
| 2.2 | `arch_syscall.rs` | **DONE** | Writes `p_cr3`/`p_cr3_v` on exec'd process's Proc struct |
| 2.3 | `syscall.S` | **DONE** | Exec target return loads `p_cr3` from Proc (offset 0x80) and switches CR3 |

**What `exec_setup_new_page_table` does:**

1. Allocates PML4, PDP, PD (zeroed 4KB pages via `alloc_phys_page`)
2. Deep-copies BOOT_PD into new PD (512 entries, kernel identity map)
3. Links PML4[0] → new PDP → new PD
4. Shares PML4[L4_SLOT_DIRECT] → BOOT_PDP (kernel high mapping)
5. Shares PDP[3] → BOOT_PD3 (APIC MMIO)
6. Splits 2MB PDEs at code/stack ranges into 4KB page tables
7. Allocates private frames for each code page, copies data via
   `core::ptr::copy_nonoverlapping` (through BOOT_CR3 identity map)
8. Remaps each virtual page to its private frame in the new page table
9. Writes `p_cr3` = physical address of new PML4, `p_cr3_v` = virtual

**Exec target CR3 restore in `syscall.S`:**

```asm
    ; Exec target — restore per-process CR3 from CURRENT_PROC
    lea     rax, [rip + CURRENT_PROC]
    mov     rax, [rax]
    test    rax, rax
    jz      4f
    mov     rax, [rax + 0x80]   ; p_cr3 at Proc offset 0x80
    test    rax, rax
    jz      4f
    mov     cr3, rax
4:
    mov     rsp, rdx            ; user RSP (preserved in RDX from exec target)
    ...
```

The CR3 switch happens BEFORE `mov rsp, rdx` to keep the kernel stack
accessible in case of interrupt.

### Phase 3: Fork support ✅ (DONE)

**Goal:** When a process calls `fork()`, the child gets its own page
table with private copies of the parent's pages.

**Implementation:** `pt_new_for_fork` in `servers/src/vm/proc.rs`:

1. Gets the parent's CR3 via `kernel::vm::vm_get_addrspace(parent_ep)`
2. Walks the parent's page table (via identity map):
   - PML4[0] → PDP[0] → PD (covers 0-1GB)
   - For each PDE with PG_U set:
     - If still a 2MB huge page (PG_PS): child keeps shared identity mapping
     - If split into 4KB page table: walk all 512 PTEs
       - For each PTE with PG_U + PG_P: private-copy to child
3. Allocates new physical frames, copies data from parent's physical frames
   through the identity map
4. Splits child's PDE and remaps each page to its private frame
5. Binds the child's page table via `pt_bind`

**Updated `do_fork` in `servers/src/vm/call.rs`:**
```rust
proc::pt_new(child);             // alloc PML4, copy kernel mappings
proc::pt_new_for_fork(child, parent_ep);  // private-copy parent pages
proc::pt_bind(child);            // write p_cr3 on kernel's Proc
```

**Helper added:** `kernel::vm::vm_get_addrspace(ep)` returns the physical
address of a process's PML4, or 0 if none.

### Phase 4: Regressions to check

| Check | What to verify |
|-------|---------------|
| IPC dispatch with per-process CR3 | The `asm_exec_handler` reads `caller.p_reg` fields. After Phase 1, the caller runs under per-process CR3. The Proc struct (in kernel BSS, identity-mapped) is accessible. The message buffer (`m_ptr`) is user-space, accessible through per-process CR3. |
| Timer interrupt during per-process CR3 | The timer handler calls `apic::eoi()`. With BOOT_PD3 shared via PDP[3] (our recent fix), the per-process page table maps APIC MMIO. ✓ |
| `delivermsg_check` with per-process CR3 | Writes to the target process's message buffer (user-space address). Works through BOOT_CR3, may not be visible if target runs on per-process CR3. Requires fixing. |
| fork() with per-process CR3 | Child needs its own page table. Phase 3 handles this. |

### Phase 5: Map kernel BSS with proper permissions ✅ (DONE)

**Changes:**

1. **Enabled EFER_NXE** in `cstart.rs::enable_long_mode()` — the NX bit
   (bit 63 in page table entries) is now active on x86_64.

2. **Defined `PG_NX`** in `arch-x86_64/src/pte.rs` (alias for `PG_I`, bit 63).

3. **`pt_mapkernel`** (`servers/src/vm/pagetable.rs`) now splits the 2MB
   PDE covering kernel text/data/BSS (at 0x200000) and sets `PG_NX` on
   BSS pages (from `__bss_start` to `__bss_end`), preventing accidental
   code execution from kernel data pages. The global bit (PG_G) is
   cleared on these entries so TLB invalidates correctly on CR3 switch.

4. The identity map deep copy of BOOT_PD still provides the base mapping.
   Phase 5 adds fine-grained permissions on top, only for the kernel
   range that matters.

**Current kernel page permissions in per-process page tables:**

| Range | Type | Permissions |
|-------|------|-------------|
| 0x000000–0x1FFFFF | User identity | RWX (unchanged) |
| 0x200000–kernel_start | Kernel text | Split to 4KB, read-only, exec (no PG_NX) |
| kernel_start–__bss_start | Kernel text/rodata/data | Split to 4KB, readable/writable, exec |
| __bss_start–__bss_end | Kernel BSS | Split to 4KB, readable/writable, NX |
| 0x400000–user_top | User identity | RWX (unchanged) |
| KERNBASE+offset | Kernel high map | 2MB pages, RW (shared BOOT_PDP) |
| PDP[3] | APIC MMIO | RW (shared BOOT_PD3)

---

## Summary of Changes

| # | File | Change | Phase | Status |
|---|------|--------|-------|--------|
| 1 | `syscall.S` | Save incoming CR3 at entry before BOOT_CR3 load | 1.1 | **DONE** |
| 2 | `syscall.S` | Restore saved CR3 on normal return path | 1.2 | **DONE** |
| 3 | `syscall.S` | Fix FORK offset `[rsp+16]` → `[rsp+24]` for user RSP | 1.3 | **DONE** |
| 4 | `arch_syscall.rs` | `exec_setup_new_page_table` — create per-process PT with private copies | 2.1 | **DONE** |
| 5 | `arch_syscall.rs` | Write `p_cr3` on exec'd process's Proc entry | 2.2 | **DONE** |
| 6 | `syscall.S` | Restore `p_cr3` on exec target return | 2.3 | **DONE** |
| 7 | `kernel/src/ipc.rs` | Switch to target's per-process CR3 in `delivermsg` before writing to user buffer | 4 | **DONE** |
| 8 | `servers/src/vm/proc.rs` | `pt_new_for_fork` — create child page table with private copies of parent's pages | 3 | **DONE** |
| 9 | `servers/src/vm/call.rs` | `do_fork` calls `pt_new` + `pt_new_for_fork` + `pt_bind` | 3 | **DONE** |
| 10 | `kernel/src/vm.rs` | Add `vm_get_addrspace` helper | 3 | **DONE** |
| 11 | `cstart.rs` | Enable EFER_NXE at boot | 5 | **DONE** |
| 12 | `servers/src/vm/pagetable.rs` | Split kernel 2MB PDE, mark BSS pages as NX | 5 | **DONE** |
| 13 | `arch-x86_64/src/pte.rs` | Add `PG_NX` constant | 5 | **DONE** |

---

## Current Implementation Status

| Step | Status | Notes |
|------|--------|-------|
| exec_setup_new_page_table | **DONE** | Creates per-process PT with private code/stack copies. Called from asm_exec_handler after load_elf. |
| Exec target CR3 restore | **DONE** | syscall.S loads p_cr3 from CURRENT_PROC before switching to user RSP. |
| Phase 1 — Save CR3 at entry | **DONE** | `mov rax, cr3; push rax` after pushing rcx. |
| Phase 1 — Restore CR3 on normal return | **DONE** | `mov rax, [rsp+0]; mov cr3, rax` on normal return. |
| Phase 1 — Fix FORK offsets | **DONE** | `[rsp+24]` instead of `[rsp+16]` for user RSP. |
| Phase 4 — delivermsg CR3 switch | **DONE** | `delivermsg()` switches to target's p_cr3 before writing to user buffer. |
| Phase 3 — Fork support | **DONE** | `pt_new_for_fork` walks parent page table, private-copies user pages, binds child. |
| Phase 5 — Kernel BSS permissions | **DONE** | EFER_NXE enabled, kernel 2MB PDE split, BSS pages marked NX in per-process PT. |

## Key Architectural Decisions

### Why load_elf writes through BOOT_CR3 (identity map)

`load_elf` parses ELF headers and writes segments to virtual addresses
(e.g. 0x1000000). These writes happen through the identity map (BOOT_CR3),
which directly maps physical memory starting at 0. Writing to virtual
address 0x1000000 under BOOT_CR3 writes to physical address 0x1000000.

The private per-process page table is constructed AFTER load_elf:
1. Create a fresh page table hierarchy (PML4 → PDP → PD)
2. Deep-copy the boot PD identity entries
3. Split 2MB PDEs at the relevant ranges
4. Allocate new physical frames, copy identity-mapped data into them
5. Remap the virtual addresses to point to the new private frames
6. Write `p_cr3` on the Proc struct

This means the identity-mapped originals are "source material" — they
hold the data that gets copied into private frames. The exec'd process
never touches the identity-mapped originals again (after switching to
per-process CR3). But without Phase 1, the first syscall destroys this.

### Why the exec'd process needs Phase 1

Flow after Phase 1 is implemented:

```
1. exec target return → CR3 switched to per-process PT
2. Process runs in private address space (code and stack are private copies)
3. Process makes write() syscall:
   a. Entry: saves per-process CR3, loads BOOT_CR3
   b. Handler reads user buffer through BOOT_CR3 identity map
   c. Handler writes to file descriptor (works normally)
   d. Return: restores per-process CR3
4. Process continues in private address space ✓
```

Without Phase 1, step 3d never happens, and the process runs on BOOT_CR3
after the first syscall.

### Current limitations

- The write handler reads user data through BOOT_CR3. After Phase 1,
  the identity-mapped user data is the OLD data (from the identity-mapped
  originals), not the private copies. For the exec'd process, this is
  actually correct: load_elf wrote the new binary to the identity-mapped
  pages, and the private copies are made from those. So the write handler
  reads the new binary's data through the identity map. ✓

- For a forked process (Phase 3, not yet done), the identity-mapped
  originals may differ from the per-process private copies (if the child
  modifies its pages). The write handler would then read stale data.
  This is a known limitation that must be addressed in Phase 4.
