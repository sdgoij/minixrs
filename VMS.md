# VM Server Implementation Plan

Based on original MINIX 3.3.0 `minix/servers/vm/` (35 source files).

## Architecture Overview

The VM server is a **userspace system process** that manages virtual memory for all
processes. It handles `brk`, `mmap`, `munmap`, fork/exec memory setup, page fault
resolution, and physical page allocation. It communicates with PM, VFS, RS, and the
kernel via IPC.

### Data Flow

```
User process                   VM server                  Kernel
     │                            │                          │
     ├─ brk(addr) ───────────────►│                          │
     │  (libc → minix_rt          │                          │
     │   → SENDREC to VM)         │                          │
     │                            ├─ validate region extent  │
     │                            ├─ alloc physical pages    │
     │                            ├─ update page table ─────►│
     │                            │  (sys_vmctl)             │
     │                            ├─ reply OK ──────────────►│
     │◄── return 0 ───────────────┤                          │
```

### Process Table (`struct vmproc`)

Each process known to VM has a `vmproc` entry:

| Field | Purpose |
|-------|---------|
| `vm_endpoint` | Kernel endpoint of the process |
| `vm_flags` | VMF_INUSE, VMF_EXITING |
| `vm_slot` | Index into vmproc table |
| `vm_pt` | Page table root (physical address of PML4) |
| `vm_regions` | AVL tree of `vir_region` objects |
| `vm_total` / `vm_total_max` | Memory accounting |

### Memory Region (`struct vir_region`)

A contiguous range of virtual addresses with a memory type:

| Field | Purpose |
|-------|---------|
| `vaddr` | Start virtual address |
| `length` | Size in bytes |
| `flags` | VR_WRITABLE, VR_ANON, VR_DIRECT |
| `mem_type` | How backing memory is provided (anon, direct, file, shared) |
| `physblocks[]` | Array of physical block references |

The original has 7 memory types: `mem_anon` (anonymous/zero-fill), `mem_directphys`
(direct physical mapping), `mem_shared`, `mem_file` (file-backed), `mem_cache`,
`mem_anon_contig`. For boot, we only need `mem_anon` (brk heap) and `mem_directphys`
(kernel-mapped boot memory).

---

## Phased Implementation

### Phase 1: Minimal VM Server — `brk` only (this session)

**Goal:** Boot processes can allocate heap memory. MFS `lmfs_buf_pool(512)`
succeeds.

**New file:** `crates/servers/src/vm/vm_server.rs`

#### 1.1 VM process table

```rust
/// Number of process slots in the VM table.
const VM_NR_PROCS: usize = 256;

struct VmProc {
    vm_endpoint: i32,       // kernel endpoint
    vm_flags: u32,          // VMF_INUSE
    /// Data segment region: [ds_start, ds_end)
    ds_start: u64,          // start VA of data segment
    ds_end: u64,            // end VA (brk)
    ds_max: u64,            // maximum allowed brk (stack gap protection)
}

static VM_PROCS: VmProcTable = ...;
```

#### 1.2 `brk` handling

```
do_brk(msg) → real_brk(vmp, new_addr):
    1. Validate new_addr >= ds_start && new_addr < ds_max
    2. If new_addr > ds_end:
       a. Compute pages_needed = (new_addr - ds_end) / PAGE_SIZE
       b. Allocate physical pages (call kernel alloc_phys_contig)
       c. Map pages in process page table (call kernel to update PT)
    3. Update ds_end = new_addr
    4. Return OK
```

#### 1.3 Kernel interface

VM uses these kernel calls (via `kernel_call`):

| Call | Purpose |
|------|---------|
| 34 (SETGRANT) | Already implemented — VM registers its grant table |
| `sys_vmctl` | Update a process's page table (map/unmap pages) |
| `alloc_phys_contig` | Allocate contiguous physical pages |

For minimal Phase 1, we need a kernel handler that:
1. Allocates physical pages from the boot allocator
2. Maps them into the target process's page table at a given VA

This is essentially what `boot_create_restricted_page_table` already does during
boot. We need a runtime version accessible via `kernel_call`.

#### 1.4 Boot sequence change

1. Kernel starts VM as a boot process (it already does — `/sbin/vm`)
2. VM's `sef_cb_init_fresh`:
   a. Initialises the vmproc table
   b. Calls `sys_getimage` / reads boot image table from kernel
   c. For each boot process, records its code/stack/data region boundaries
   d. Sets `ds_start = &_end` (end of BSS) and `ds_end = ds_start` (no heap yet)
   e. Exits init callback
3. Processes call brk → SENDREC(VM, VM_BRK) instead of kernel syscall

#### 1.5 Userspace brk change

In `minix-rt` or `minix-std`:
```rust
pub unsafe fn brk(addr: *const u8) -> i64 {
    // Build a VM_BRK message
    let mut msg = Message::zeroed();
    msg.m_type = VM_RQ_BASE + 2;  // VM_BRK
    msg.payload.lc_vm_brk.addr = addr;
    // Send to VM server
    let r = sendrec(VM_PROC_NR, &mut msg);
    if r != OK { return -1; }
    msg.payload.lc_vm_brk.result as i64
}
```

---

### Phase 2: Page Table Management

**Goal:** VM can create and modify per-process page tables at runtime.

#### 2.1 Kernel support for page table ops

Add a kernel call handler that:
- `VM_MAP_PAGE(pt_phys, va, pa, flags)` — map a single 4K page
- `VM_UNMAP_PAGE(pt_phys, va)` — unmap a page
- `VM_ALLOC_PAGES(count)` → physical address of contiguous pages
- `VM_FREE_PAGES(pa, count)` — return pages to free pool

#### 2.2 Region abstraction

```rust
struct VirRegion {
    vaddr: u64,
    length: u64,
    flags: u32,        // VR_WRITABLE, VR_ANON, VR_DIRECT
    phys_pages: Vec<u64>,  // physical addresses of backing pages
}
```

For Phase 2, use a flat `Vec<VirRegion>` per vmproc instead of an AVL tree.
AVL is needed for performance with many regions but boot only has 2-3 per
process (code, data, stack).

---

### Phase 3: `mmap` / `munmap`

**Goal:** Generic anonymous memory mapping.

```
do_mmap(vmp, vaddr, len, flags):
    1. Validate vaddr and len
    2. Create a new VirRegion { vaddr, len, VR_WRITABLE|VR_ANON }
    3. Allocate physical pages (lazy — defer until page fault)
    4. Insert region into vmp's region list
    5. Return OK / vaddr

do_munmap(vmp, vaddr, len):
    1. Find region containing vaddr
    2. Free physical pages
    3. Unmap pages from page table
    4. Remove region (or split if partial)
    5. Return OK
```

---

### Phase 4: Fork / Exec Support

**Goal:** PM can create child processes via fork and exec.

#### 4.1 Fork

```
do_fork(vmp_parent, vmp_child):
    For each region in parent:
        - Allocate new physical pages for child
        - Copy data from parent's physical pages to child's
        - Map new pages in child's page table
    Return child's new page table root
```

#### 4.2 Exec

```
do_exec(vmp, elf_data):
    1. Parse ELF header
    2. Create code region from PT_LOAD segments
    3. Allocate physical pages, load segments
    4. Map code pages in page table
    5. Create stack region, allocate stack pages
    6. Set up initial TrapFrame (entry, sp)
    7. Return new page table root and entry point
```

---

### Phase 5: Page Fault Handling

**Goal:** Handle `VM_PAGEFAULT` messages from the kernel for lazy allocation.

The kernel delivers page faults as IPC notifications to VM. VM must:
1. Look up the faulting address in the vmproc's region list
2. If the region exists and the fault is valid (e.g., write to writable region):
   a. Allocate a physical page
   b. Map it in the page table
   c. Zero-fill if anonymous
   d. Resume the process via `sys_vmctl`
3. If the fault is invalid (e.g., write to read-only, unmapped address):
   a. Send SIGSEGV to the process via PM

---

### Phase 6: Exit / Cleanup

**Goal:** Free all resources when a process exits.

```
do_exit(vmp):
    For each region in vmp:
        - Free physical pages back to the allocator
    Free the page table pages
    Mark vmproc slot as free
```

---

## Files to Create / Modify

| File | Purpose | Phase |
|------|---------|-------|
| `crates/servers/src/vm/vm_server.rs` | VM main loop + dispatch | 1 |
| `crates/servers/src/vm/vm_proc.rs` | vmproc table + init | 1 |
| `crates/servers/src/vm/vm_break.rs` | do_brk / real_brk | 1 |
| `crates/servers/src/vm/vm_region.rs` | VirRegion abstraction | 2 |
| `crates/servers/src/vm/vm_mmap.rs` | do_mmap / do_munmap | 3 |
| `crates/servers/src/vm/vm_forkexec.rs` | do_fork / do_exec | 4 |
| `crates/servers/src/vm/vm_pagefault.rs` | do_pagefaults | 5 |
| `crates/servers/src/vm/vm_exit.rs` | do_exit | 6 |
| `crates/kernel/src/system.rs` | Kernel page table / phys alloc handlers | 1-2 |
| `crates/minix-rt/src/lib.rs` | `brk()` → SENDREC to VM | 1 |
| `crates/minix-std/src/lib.rs` | mmap/munmap wrappers | 3 |
| `crates/kernel-boot/src/main.rs` | Add VM to boot_procs | 1 |

## Key Kernel Primitives Needed

These must be accessible from userspace (VM) via existing `kernel_call` mechanism:

1. **`alloc_phys_contig(pages)` → phys_addr** — allocate N contiguous physical pages from the boot free pool. Already exists: `kernel::hal::alloc_phys_contig`.

2. **`map_page(cr3, va, pa, flags)` → OK/error** — map a 4K page in the given page table. Already exists: `kernel::pagetable::map_page`.

3. **`free_phys_contig(pa, pages)`** — return physical pages to free pool. Already exists.

These three functions need kernel call numbers added to the dispatch table so VM can invoke them.

## Phase 1 Success Criteria

- VM server compiles, boots as a server process
- INIT calls `brk()` which goes through VM instead of kernel
- MFS calls `lmfs_buf_pool(512)` which successfully allocates 512 buffer headers
- Buffer pool initialisation completes without panic
