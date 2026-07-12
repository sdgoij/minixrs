# Fork/Exec/Waitpid Hang Investigation

## Current State (2026-07-12)

**Fork works.** The child page table is created, the child is enqueued, and
`restore()` switches to the child. The child executes user-mode instructions
(confirmed by QEMU `-d int` showing CPL=3 RIP changes).

**The system then hangs** — the kernel enters `hlt()` idle loop because no
process is runnable. The child makes no syscalls (no `S` or `1`/`2` diagnostics
appear after `restore`), and no other processes are picked by the scheduler.

## Fixed Bugs This Session

### VM Physical Memory Model
- Added `vm_mappage`/`vm_unmappage` for safe physical page access from user mode
- Added `VM_PAGING_FORK` (kernel call 62, subcmd 7) — single ring-0 kernel call
  for page table cloning, replacing the broken multi-call user-mode approach
- Rewrote `pt_new_for_fork` to use `VM_PAGING_FORK`

### Scheduler / Fork
- Added `PREEMPTIBLE` flag to boot process privileges
- Added `PREEMPTED` flag to `do_fork_handler` clear list (child inherits from parent)
- Set child's `p_cpu_time_left` to 50ms (was 0, causing immediate quantum expiry)
- Added `WAITING` flag in PM's `handle_waitpid` (matching C code)
- Added `tell_parent` / `wait_test` in PM's `do_exit` to unblock waiting parent
- Fixed `wait_test` to check `WAITING` flag (was only checking `mp_wpid`)

### Kernel Call Dispatch
- Fixed extra brace in `VM_PAGING_MAP` handler
- Made `alloc_pt_page` pub(crate) for internal use
- Removed all debug diagnostics from `syscall_entry` and `kernel_call_dispatch`

## Remaining Blocker

After `restore()` switches to the child, the child runs a few user-mode
instructions (confirmed by QEMU `-d int` showing CPL=3 RIP at 0x1000000,
0x1008fa6, 0x1000e6c) but never makes a syscall. Then the kernel enters
`hlt()` with CPL=0, `II=0` (interrupts disabled), and the system hangs.

Possible causes:
1. Child page table maps code pages incorrectly (TLB miss #PF on first
   instruction fetch). The #PF handler might not deliver SIGSEGV properly.
2. Child's stack is not mapped (argv pointers dereference causes #PF).
3. Child jumps to an invalid address (RIP or RSP corrupted by fork).

The `-d int` QEMU output showed:
- `RIP=0x1000000 CPL=3` — user code at entry point
- `RIP=0x1008fa6 CPL=3` — moved further
- `RIP=0x1000e6c CPL=3` — same address, different RFLAGS (ZF changed)
- `RIP=0x211bcf CPL=0` — kernel hlt address

The ZF change between `0x00000246` and `0x00010246` suggests a `cmp` or `test`
instruction executed at 0x1000e6c followed by a `jz`/`jne`. This is consistent
with the fork RAX check (`cmp rax, 0; jne parent`).

No `#PF` or `#GP` exceptions were logged, suggesting the child's page table
IS valid for the first few instructions. The child should be able to reach
`exec_replace` (syscall 61).

## Next Steps

1. **Run with `-d int` without grep filtering** to see the full exception trace
   when the child runs. Look for any exception (even unlabeled ones like `T=`
   for timer) that might indicate a fault.

2. **Check if the child's `syscall` instruction works**. The child tries to
   call syscall 61 (exec_replace). If the syscall mechanism fails (bad LSTAR
   MSR, bad kernel stack), the child would #GP. But the log shows no #GP.

3. **Check if the kernel's `syscall_handler_c` processes the child's syscall**.
   Add a minimal diagnostic right at the entry of `syscall_handler_c` that
   prints the syscall number for CPL=3 calls. If the child makes a syscall
   but the handler crashes, we'd see the syscall number but no return.

4. **Test with a simpler command** like `cat /bin/sh` that goes through the
   existing VFS read path instead of trying to exec a non-existent binary.
