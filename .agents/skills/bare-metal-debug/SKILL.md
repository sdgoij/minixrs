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

A #GP (vector 13) is the most common crash. The #GP handler prints `G` followed by a
structured diagnostic line to COM1, then halts.

### Output Format

The handler outputs a single line:
```
G{err} {rip} {cs} {rfl} {tir} {tcs} {trf} {trsp} {tss}\r\n
```

| Field | Width | Description |
|-------|-------|-------------|
| `G` | 1 char | #GP marker |
| `err` | 4 hex | Error code |
| `rip` | 16 hex | Instruction pointer at fault |
| `cs` | 4 hex | Code segment at fault |
| `rfl` | 8 hex | RFLAGS at fault |
| `tir` | 16 hex | Timer ISR interrupted RIP |
| `tcs` | 4 hex | Timer ISR interrupted CS |
| `trf` | 8 hex | Timer ISR interrupted RFLAGS |
| `trsp` | 16 hex | Timer ISR interrupted stack pointer |
| `tss` | 4 hex | Timer ISR interrupted stack segment |

The timer ISR fields are only meaningful when the #GP occurred during the timer
ISR's iretq. Otherwise they contain stale stack data.

### Decoding the #GP Error Code

The #GP error code is the first 4 hex digits after `G`.
Example: `G0010` means error code 0x0010 (GDT index 2 = GUDATA_SEL).

Error code meanings:

- **0x00000000**: Reserved RFLAGS bit set (VM=bit 17, or bits 31:22, 17:12, 16:14 non-zero), OR non-canonical RIP. Most common on iretq to ring 3.
- **0x000000XX**: Segment selector error. XX is the selector. Check GDT entry at `idx = XX >> 3`.
- **EXT=0x01**: External event
- **IDT=0x02**: Fault refers to IDT entry
- **TI=0x04**: Fault refers to LDT (otherwise GDT)

### Error Code 0x0010: SS vs CS Ambiguity

An iretq #GP with error code **0x0010** (GDT index 2 = GUDATA_SEL)
is **ambiguous** between two possible causes:

| Possible Cause | What Happens |
|---------------|--------------|
| **SS problem** | SS=0x0010 with RPL=0 loaded while CS.RPL=3 → SS.RPL mismatch |
| **CS problem** | CS=0x0010 loaded into CS — a **data segment descriptor** (type=2)
  cannot be loaded into a code segment register → descriptor type violation |

To distinguish:

1. **Add a diagnostic before iretq** that dumps the SS field (at `[rsp+32]`
   in a 5-entry user-mode frame).
2. If SS is already correct (0x0013 with RPL=3) but the #GP with error
   code 0x0010 persists, **CS is the culprit** — the iretq is trying
   to load a data segment selector into CS.
3. The most robust fix is to **pop all 5 iretq frame entries and rebuild**
   them with hardcoded correct segment selectors, rather than patching
   individual fields:
   ```asm
   pop    rcx              ; RIP
   pop    rax              ; CS (discarded — use 0x001B below)
   pop    r11              ; RFLAGS
   pop    r10              ; old_RSP
   add    rsp, 8           ; skip old_SS (discarded)
   push   0x0013           ; SS = GUDATA_SEL | RPL=3
   push   r10              ; old_RSP
   push   r11              ; RFLAGS
   push   0x001B           ; CS = GUCODE_SEL | RPL=3
   push   rcx              ; RIP
   iretq
   ```

Actual case from this project: `mov [rsp+32], 0x0013` (patching SS)
did NOT fix the crash because the real problem was CS=0x0010, not SS.
The diagnostic dump showed SS was already correct (0x13) yet the #GP
persisted — proving the problem was elsewhere in the frame.

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

## Sending Serial Input via stdin

When testing the shell interactively (e.g., running an unknown command to
trigger fork/exec/exit/waitpid), you need to send input to QEMU's serial
port. Piping directly through `just run` does **not** work because the
Justfile uses `-nographic` which sets `-serial mon:stdio` — the QEMU
monitor multiplexes on stdin and interferes with serial input.

### Working approaches

**Approach 1: Run QEMU directly with `-nographic -monitor none`**

```
(sleep 3 && echo "abc"; sleep 3) | qemu-system-x86_64 -nographic -monitor none \
    -m 256M -no-reboot -kernel target/trampoline.elf \
    -device loader,file=target/kernel.bin,addr=0x200000
```

This keeps the serial port on stdio but removes the monitor from the mux.

The `sleep 3` gives the kernel time to boot before input arrives.

**Approach 2: Modify Justfile**

Edit the `run` recipe to use `-nographic -monitor none` instead of just `-nographic`.

### What DOESN'T work

- `(sleep N; echo ...) | just run` — the `-nographic` default leaves the monitor
  on stdio, which steals bytes from the serial input.
- `-display none -serial stdio` — creates a serial port but may not configure
  COM1 interrupts correctly for this kernel.
- `echo "cmd" > /proc/PID/fd/0` — not applicable on Windows (WSL may work).

### Why this matters

When debugging fork/exec/exit/waitpid hangs, you need to:
1. Boot the kernel with shell at `# ` prompt
2. Send an unknown command (e.g., `abc`)
3. Observe the shell's response (`sh: abc: not found`) or the absence thereof
4. Check kernel diagnostic characters (`!` = child found by SYS_GETKSIG, `?` = none found)

Without proper serial input, step 2 fails and the test is useless.

### QEMU `-d int,cpu_reset`

Run with interrupt tracing to see every exception and the final crash state:

```
qemu-system-x86_64 -nographic -monitor none -m 256M -no-reboot -d int,cpu_reset \
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

## LLDB Remote Debugging with QEMU

QEMU can act as a GDB server, allowing LLDB to set breakpoints, step through
code, and inspect memory/registers — without adding temporary diagnostic prints.

### Quick start

```bash
# Terminal 1: start QEMU with GDB server, frozen at boot
just debug

# Terminal 2: connect LLDB
lldb target/x86_64-pc-minix/release/kernel-boot
(lldb) gdb-remote 127.0.0.1:1234
(lldb) b kmain
(lldb) c
```

Or connect to an already-running system (no `-S`):

```bash
# Terminal 1 (after build):
qemu-system-x86_64 -display none -serial stdio -m 256M -no-reboot -s \
    -kernel target/trampoline.elf \
    -device loader,file=target/kernel.bin,addr=0x200000 &

# Terminal 2 (after boot reaches shell):
lldb target/x86_64-pc-minix/release/kernel-boot
(lldb) gdb-remote 127.0.0.1:1234
(lldb) b pm_dispatch
(lldb) c
```

### Symbol files

| Component | ELF path |
|-----------|----------|
| Kernel    | `target/x86_64-pc-minix/release/kernel-boot` |
| PM        | `target/x86_64-pc-minix/release/pm` |
| VFS       | `target/x86_64-pc-minix/release/vfs` |
| VM        | `target/x86_64-pc-minix/release/vm` |
| SCHED     | `target/x86_64-pc-minix/release/sched` |
| TTY       | `target/x86_64-pc-minix/release/tty` |
| MFS       | `target/x86_64-pc-minix/release/mfs` |

All server ELFs are statically linked and **not stripped** (have symbol tables).
Since servers all load at virtual address 0x1000000, debug one at a time:

```
lldb target/x86_64-pc-minix/release/pm
(lldb) gdb-remote 127.0.0.1:1234
(lldb) b do_fork
(lldb) c
```

### Useful LLDB commands

| Command | What it does |
|---------|-------------|
| `gdb-remote localhost:1234` | Connect to QEMU's GDB server |
| `b kmain` | Set breakpoint at `kmain` function |
| `b 0x200400` | Set breakpoint at an address |
| `c` | Continue execution |
| `n` | Step over (next line) |
| `s` | Step into |
| `si` | Step one instruction |
| `register read` | Dump all CPU registers |
| `register read rflags` | Read RFLAGS specifically |
| `x/10gx $rip` | Examine 10 qwords at RIP |
| `x/10i $rip` | Disassemble 10 instructions at RIP |
| `memory read -f x -s 8 -c 64 0x200000` | Read 64 qwords as hex |
| `p/x *(uint64_t*)0x200000` | Evaluate C-like expression (LLDB) |
| `frame variable` | Show local variables (if DWARF available) |
| `thread backtrace` | Show stack backtrace |
| `target create -s symtab <elf>` | Load symbols from an ELF file |
| `image list` | List loaded shared libraries/symbol files |

### Finding symbol addresses

Since release builds strip DWARF but keep symbol tables, use `rust-nm` to
find mangled function names for breakpoints:

```bash
# Kernel symbols
rust-nm -n target/x86_64-pc-minix/release/kernel-boot | grep do_fork

# Server symbols (separate ELFs)
rust-nm -n target/x86_64-pc-minix/release/pm | grep handle_fork
rust-nm -n target/x86_64-pc-minix/release/vm | grep vm_clone
```

This is required because Rust mangling changes function names. Use the
mangled name or set a breakpoint by address:

```
(lldb) b 0x2043e0    # kernel do_fork_handler
(lldb) b 0x1000170   # PM handle_fork
(lldb) b 0x1000e20   # VM do_fork
```

### Debugging practice: check CPU state on hang

When the system hangs, connect LLDB and examine the current state:

```
(lldb) gdb-remote 127.0.0.1:1234
(lldb) bt                  # backtrace
(lldb) register read rip cr2 eflags   # fault address + CPU flags
(lldb) x/10i $rip          # disassemble at current RIP
(lldb) memory read -f x -s 8 -c 16 $rsp  # examine stack
```

- RIP tells you what code was running
- CR2 gives the fault address (on page fault)
- eflags bit 9 (IF=0) means interrupts disabled — CPU may be halted
- Compare RIP against `rust-nm` output to identify the function

### Building with debug info

The default `--release` build strips DWARF info but keeps the symbol table.
For source-level debugging with line numbers, build the kernel with debug
symbols:

```
# Temporarily edit Justfile or run directly:
cargo build -p kernel-boot --target x86_64-pc-minix.json \
    -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
```

Without debug info, LLDB can still resolve function names from the symbol
table and set breakpoints by name, but `frame variable` and line-level
stepping won't work.
