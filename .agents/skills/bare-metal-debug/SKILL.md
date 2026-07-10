---
name: bare-metal-debug
description: Debugging methodology for bare-metal MINIX/Rust OS development. Use when investigating hangs, crashes, silent failures, or corrupted data in the kernel or server processes running on QEMU.
---

# Bare-Metal Debug Methodology

## Core Principle: Cheapest Test First

Every QEMU cycle costs ~30-120 seconds. **Do not theorize for 10 minutes what you can disprove in one boot test.** Before writing a paragraph of analysis, write 5 lines of boot test code.

```
Bad:  "virtual_copy must be broken because CR3 switches..."
Good: "Let me add a test that reads MFS's delivermsg and dumps bytes 8-31"
```

## Debugging Ladder - Start at the Bottom

When data is wrong, start from the **source** and work outward. Do not skip rungs.

```
1. Is the source data correct?      (FS image, initramfs, hardcoded constant)
2. Is the reading mechanism intact?  (function called? returns OK?)
3. Is the transport working?         (IPC send/receive, shared memory, grants)
4. Is the receiver parsing right?    (struct offsets, endianness, message layout)
5. Is the consumer using it right?   (caller checks return value? casts correctly?)
```

Concrete example from this project - root directory size was zero:

| Rung | Question | How to Test | Cost |
|------|----------|-------------|------|
| 1 | Does the FS image have a valid root inode? | `od -A x target/minixfs.img` at inode block offset | 0s (host) |
| 2 | Does MFS call `rw_inode`? | Grep for `rw_inode` in `crates/fs/src/mfs/inode.rs` | 0s (grep) |
| 3 | Does the RPC reply contain the right bytes? | Add `serial_write` of `p_delivermsg` bytes 8-31 in boot test | 1 QEMU cycle |
| 4 | Does VFS parse the reply at the right offsets? | Check `Message` struct layout and `PAYLOAD_OFF` values | 0s (read code) |
| 5 | Does `mount_root` check the return value? | Read `mount_root()` in VFS | 0s (read code) |

## Boot Tests Are Your Fastest Probe

The `boot-test` feature (`just test-boot`) runs assertions inside the kernel before any shell starts. Use it to:

- **Check a function return**: `test_vfs_reply_from_mfs()` reads VFS's `p_delivermsg.m_type` to confirm OK
- **Dump raw bytes**: Add a `for i in 8..32 { print_hex(msg[i]); }` loop - you get concrete data, not speculation
- **Test one hypothesis per cycle**: Add one test, run it, see the result. Don't add 5 tests and wonder which failed
- **Remove it after**: Debug tests are temporary. Remove or gate them once the issue is resolved

### Boot Test Patterns

```rust
// Pattern 1: Check a return value / status code
fn test_something_worked() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(SOME_PROC_NR);
        if (*rp).p_delivermsg.m_type != OK {
            serial_write("  FAIL: ...\r\n");
            return 1;
        }
    }
    0
}

// Pattern 2: Dump raw bytes (expensive - remove after debugging)
// serial_write("  DBG: bytes 8-15: ");
// for i in 8..16 { print_hex(*msg.add(i)); serial_write(" "); }
// serial_write("\r\n");

// Pattern 3: Compare two values you expect to match
if expected != actual {
    serial_write("  FAIL: expected "); print_dec(expected);
    serial_write(" got "); print_dec(actual); serial_write("\r\n");
    return 1;
}
```

## Avoid Tunnel Vision

When a symptom points to a complex subsystem (page tables, IPC, grants), **actively list 3-5 simpler explanations before investigating the complex one.**

Counter-example from this session:
- Symptom: `req_getdents` returns zeros
- Jumped to: `virtual_copy` / grant table / CR3 switching
- Written: multi-page Blocker 3 analysis
- Actual cause: `rw_inode` never called + builder didn't set inode fields
- Verification cost: one grep + one boot test

**Checklist before investigating any complex subsystem:**

- [ ] Is the function in question actually **called**? (grep for callsites)
- [ ] Does the function exist? (grep for its definition)
- [ ] If it reads data from disk/memory, is the data populated? (hexdump the source)
- [ ] If it involves IPC, is the reply being checked for errors?
- [ ] Are struct layouts compatible? (offsets, sizes, alignment)
- [ ] Are constants correct? (feature flags, endpoint numbers, message types)
- [ ] Is there a simpler path that bypasses the complex subsystem entirely?

## Scheduling-Specific Pitfalls

### `context_stop` / Quantum Accounting

The C `switch_to_user()` calls `context_stop(p)` which:
1. Reads the TSC
2. Computes delta since last context switch (`tsc_ctr_switch`)
3. Subtracts delta from `p_cpu_time_left`
4. If quantum exhausted: `proc_no_time(p)` - either notifies SCHED server (user-scheduled)
   or renews quantum (kernel-scheduled)

If `context_stop` is a no-op stub, **no process ever exhausts its quantum**.
The scheduler's preemption mechanism never fires, and a runnable process
stays at the head of its priority queue forever.

**Check:** Is `context_stop` implemented? Is it called before `pick_proc()`
in the syscall return path?

### Priority Differentiation

The original C assigns different priorities to boot processes:
- Kernel tasks: `TASK_Q = 0` (highest)
- VM, RS: `SRV_Q = USER_Q = 7` (lower)
- Other servers (PM, VFS, MFS): priority 0 at boot (via `proc_init`),
  changed later by SCHED server to `USER_Q = 7`

If all boot processes have the same priority in the Rust port, the
`enqueue` preemption check (`rp_priority < cur_priority`) never fires,
and a process at the head of a queue can starve all others at the same
priority.

### `RTS_NO_QUANTUM` / SCHED Server Protocol Chain

The kernel sets `RTS_NO_QUANTUM` when a process exhausts its quantum
(via `notify_scheduler`), which dequeues the process. The SCHED server
must clear `RTS_NO_QUANTUM` via `SYS_SCHEDULE`. Without this, the process
is stuck forever. Check whether:
- `notify_scheduler` is ever called (`kernel_scheduler()` must return false)
- The SCHED server actually receives and handles `SCHEDULING_START`/`SCHEDULING_NO_QUANTUM`
- PM sends `SCHEDULING_START` to the SCHED server during init/fork

## When You Find One Bug, Look for a Second

If fixing one thing doesn't fix the symptom, **don't escalate complexity** - look for another simple bug at the same layer. In this session, `rw_inode` was missing AND the builder didn't populate mode/size. Either alone caused the same symptom. 

## Quick Reference: Boot Test Infrastructure

```
just test-boot          # Build + boot QEMU + run 12 assertions + exit
just run                # Normal boot (skips test, starts shell)
```

Test file: `crates/kernel-boot/src/boot_test.rs`
Test runner: `pub unsafe fn run_boot_tests() -> !` in the same file.

The kernel uses `syscall 60` (`SYS_BOOT_COMPLETE`) as a signal from VFS to trigger the test suite. The handler is in `crates/kernel-boot/src/main.rs` as `boot_test_syscall_handler`.

## Debugging General Protection Faults (#GP)

A #GP (vector 13) is the most common crash. The #GP handler in this project outputs `G` to COM1 and halts.

### Decoding the #GP Error Code

The #GP exception pushes an error code. Modify the handler to dump it. Error code meanings:

- **0x00000000**: Reserved RFLAGS bit set (VM=bit 17, or bits 31:22, 17:12, 16:14 non-zero), OR non-canonical RIP. Most common on iretq to ring 3.
- **0x000000XX**: Segment selector error. XX is the selector. Check GDT entry at `idx = XX >> 3`.
- **EXT=0x01**: External event
- **IDT=0x02**: Fault refers to IDT entry
- **TI=0x04**: Fault refers to LDT (otherwise GDT)

### Common #GP Causes on iretq to Ring 3

| Check | Stack Offset | Common Failure |
|-------|-------------|----------------|
| RIP canonical | `[RSP+0]` | Non-canonical address |
| CS valid code segment | `[RSP+8]` | DATA segment loaded into CS. Error code = CS. |
| RFLAGS reserved bits | `[RSP+16]` | Bits 31:22, 17:12, 16:14 must be 0. Error code = 0. |
| RSP canonical | `[RSP+24]` | User stack outside canonical range |
| SS valid data segment | `[RSP+32]` | SS.DPL != CS.RPL. Error code = SS. |

### The SYSRETQ Trap

sysretq does NOT validate CS/SS descriptors before loading. A misconfigured STAR MSR or GDT can load a DATA segment into CS or a DPL=0 segment as SS with RPL=3. The process will execute (64-bit mode doesn't check CS type on every fetch), but the first iretq will #GP.

**Diagnostic:** Read the interrupt frame in the timer ISR (vector 0x20).
After 4 pushes (32 bytes):
```
[RSP+32] = RIP
[RSP+40] = CS
[RSP+48] = RFLAGS
[RSP+56] = old_RSP
[RSP+64] = old_SS
```
If CS=0x1B (GUDATA_SEL) instead of 0x23 (GUCODE_SEL), SYSRETQ is loading the wrong CS.

### QEMU `-d int,cpu_reset`

Run with interrupt tracing to see every exception and the final crash state:

```
qemu-system-x86_64 -nographic -m 256M -no-reboot -d int,cpu_reset \
    -kernel target/trampoline.elf \
    -device loader,file=target/kernel.bin,addr=0x200000
```

**What the flags do:**
- `-d int` — logs every interrupt/exception as a single letter. `P`=page fault, `G`=#GP, `D`=double fault, `T`=timer. The `G` in output is from this log AND from the #GP handler.
- `-d cpu_reset` — dumps full CPU register state (EIP, CS, CR0-4, page table info) on triple fault.
- `-no-reboot` — prevents QEMU from rebooting.
- Pipe through grep: `2>&1 | grep -E "(SMM|#GP|#PF|reset)" | head -50`

### Checklist When Seeing G

- [ ] Dump the #GP error code (0 = RFLAGS/RIP, non-zero = segment selector)
- [ ] Dump the iretq frame (RIP, CS, RFLAGS, old_RSP, old_SS) before the faulting instruction
- [ ] Verify CS and SS selectors point to valid GDT entries with correct types
- [ ] Check RFLAGS bits 17:12 and 16:14 are zero
- [ ] Test with `int $0x20` (software) vs real IRQ - if one works and the other doesn't, compare privilege level, page table, or stack
- [ ] Disable the interrupt source (PIT, serial) - if crash disappears, the ISR has a bug
- [ ] If crash appears during boot before any user process, check IDT, GDT, TSS, syscall MSRs
