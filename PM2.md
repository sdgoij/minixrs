# Fork/Exec/Waitpid Hang Investigation

## Current State

The system boots to shell prompt. Typing `abc` causes the shell to call `fork()`.
**Fork works now** — the child page table is created and the child is scheduled
(diagnostic `C` from `pick_proc` confirms child found). But exec/exit/waitpid
chain hangs silently.

## What Was Fixed

### VM Physical Memory Mapping (VM_PHYS_MEM.md)

VM's `pt_new_for_fork` previously dereferenced physical addresses through the
identity map from ring 3. The boot identity map has supervisor-only pages (PG_U
cleared), making these accesses invalid.

**Initial fix**: Added `vm_mappage()`/`vm_unmappage()` and `vm_map_page_in()`
infrastructure — maps physical pages into VM's address space via kernel call
(ring 0). This worked for 6 pages then hung on the 7th `vm_map_page_in` call.
Root cause unclear (possibly stack exhaustion from nested kernel calls, or
allocator state corruption).

**Current fix**: Added `VM_PAGING_FORK` (kernel call 62, subcmd 7) — a SINGLE
kernel call that does the entire fork page table clone in ring 0. The kernel:
1. Allocates new child PML4
2. Copies kernel entries (upper 256 PML4 slots)
3. Walks parent's user page tables (ring 0, identity map accessible)
4. Maps each user page in child via `map_page()` (ring 0 allocator)
5. Returns child's CR3

This bypasses ALL user-mode page table access, eliminating the supervisor-only
identity map issue and the multi-call state corruption.

### Other fixes from this session
- `VM_SELF_CR3` stored during VM init (endpoint slot extraction fix)
- `VM_MAP_FLAGS` corrected to use `MAP_USER` (bit 2) instead of `PCD` (bit 4)
- `pt_free_internal()` for cleanup on fork failure
- `vm_map_page_in()` helper function
- `VM_PAGING_FORK` kernel call (subcmd 7)
- `alloc_pt_page()` made `pub(crate)` for kernel-internal use

## Remaining Issue: exec/exit/waitpid

After fork, the child is scheduled (`C` from `pick_proc`). The child starts
executing user code but hangs during the exec/exit/waitpid pipeline.

Likely cause: the child's first action after fork is `execvp("/bin/abc", ...)`.
This requires:
1. Child calls `SYS_EXEC` (59) → PM receives it
2. PM coordinates with VFS to open `/bin/abc`
3. VFS asks MFS to read the file → file not found → error
4. PM tells VM to destroy child's address space
5. Child calls `exit()` → sends SIGCHLD to parent
6. Parent's `waitpid()` returns with child's status

If any step in this chain hangs (e.g., PM is not receiving the exec syscall,
or VFS is blocked, or the SCHED server doesn't schedule the right process),
the system stalls.

## Files Changed

| File | Change |
|------|--------|
| `crates/servers/src/vm/mod.rs` | Added `VM_SELF_CR3`, `vm_find_hole`, `vm_mappage`, `vm_unmappage`, `vm_map_page_in`, `vm_fork_pagetable`, `VM_PAGING_FORK` |
| `crates/servers/src/vm/proc.rs` | Rewrote `pt_new_for_fork` to use `VM_PAGING_FORK` single kernel call, added `pt_free_internal` |
| `crates/kernel/src/system.rs` | Added `VM_PAGING_FORK` subcommand handler, removed kernel diagnostics |
| `crates/kernel/src/pagetable.rs` | Made `alloc_pt_page` pub(crate) |
| `crates/arch-x86_64/src/asm.rs` | Removed `syscall_entry` diagnostics |
