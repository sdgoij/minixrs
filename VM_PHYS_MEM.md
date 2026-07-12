# VM Physical Memory Mapping Infrastructure

## Problem

VM's `pt_new_for_fork` (and any code that manipulates page tables) dereferences
**physical addresses as virtual pointers** through the identity map:

```rust
let parent_pml4 = parent_cr3 as *const PtEntry;  // phys→virt via identity map
core::ptr::read(parent_pml4.add(pml4_idx));        // #PF from ring 3 on supervisor-only page
```

The boot identity map has **supervisor-only** pages (PG_U cleared). VM runs in
ring 3 and cannot access them. Early development set PG_U on some identity-map
pages as a shortcut, making it "work" accidentally.

## Solution: Match C MINIX

C MINIX's VM maintains its own page table and maps physical pages into its
address space with USER|RW permissions before accessing them:

```c
void *vm_mappages(phys_bytes p, int pages) {
    loc = findhole(pages);                                // free VA in VM's space
    pt_writemap(vmprocess, pt, loc, p, VM_PAGE_SIZE*pages,
        ARCH_VM_PTE_PRESENT | ARCH_VM_PTE_USER | ARCH_VM_PTE_RW, 0);
    sys_vmctl(SELF, VMCTL_FLUSHTLB, 0);
    return (void*) loc;                                   // VA for direct access
}
```

The kernel's `SYS_VM_PAGING_MAP`/`UNMAP` subcommands (which run in ring 0)
already provide the ability to map/unmap pages in any page table. VM just needs
to use them with its own CR3.

## Implementation

### ✅ Step 1: VM learns its own CR3

`VM_SELF_CR3` (AtomicU64) saved during `vm_init_boot()` when the kernel returns
VM's own endpoint and CR3 for slot 8 (VM_PROC_NR).

### ✅ Step 2/3: VA hole allocator + vm_mappages/vm_unmappages

```rust
static VM_SELF_CR3: AtomicU64 = AtomicU64::new(0);
static VM_NEXT_MAP_VA: AtomicU64 = AtomicU64::new(0x7F0000000000);
```

Functions added to `crates/servers/src/vm/mod.rs`:
- `vm_find_hole(pages)` — bump allocator in VM's VA space
- `vm_mappage(phys, flags)` — map single physical page into VM's address space, returns VA
- `vm_unmappage(va)` — unmap a page from VM's address space
- `vm_mappages(phys, count, flags)` — map N consecutive pages
- `vm_unmappages(va, count)` — unmap N consecutive pages
- `vm_map_page_in(cr3, va, pa, flags)` — map a page in ANY page table via kernel call (ring 0)

### ✅ Step 4: Rewrite pt_new_for_fork

`crates/servers/src/vm/proc.rs`:
- Map parent's PML4 into VM's address space → read entries via VA
- Walk each PML4, PDPT, PD, PT level by mapping each intermediate page temporarily
- For each user PTE, call `vm_map_page_in(child_pml4_pa, va, pa, flags)` — kernel call in ring 0
- The kernel's `map_page` allocates intermediate page tables (PDPT, PD, PT) from the
  **kernel's own allocator**, avoiding the broken VM-local allocator
- Unmap all temporary mappings when done

Added `pt_free_internal(pml4_pa)` for cleanup on failure (frees child's partial page table).

## Remaining

| File | Change | Status |
|------|--------|--------|
| `crates/servers/src/vm/cow.rs` | Rewrite `handle_cow_fault` to use `vm_mappages` | 🔜 On hold |
