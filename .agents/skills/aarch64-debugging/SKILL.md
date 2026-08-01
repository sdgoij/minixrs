---
name: aarch64-debugging
description: Debugging the AArch64 MINIX/Rust port on QEMU with LLDB and QEMU exception traces. Use when investigating AArch64 boot hangs, syscall livelocks, IPC receive loops, or page-table/I-cache issues that differ from the working x86/RISC-V ports.
---

# AArch64 Debugging (LLDB + QEMU)

## When to Use

- AArch64 boot hangs (after a boot message, or silently in kernel/user code).
- Syscall storms / livelocks visible in QEMU `-d int` traces.
- IPC issues (RECEIVE/SENDREC loops) on AArch64.
- Page-table or I-cache coherence problems (`svc` executed at an address whose RAM bytes are not `svc`).

x86 and RISC-V boot to a shell; AArch64 is the least-tested port. Start by comparing the AArch64 flow with `crates/kernel-boot/src/main.rs` (x86) and `crates/kernel-boot/src/riscv64.rs` (RISC-V) — they are the reference implementations.

## Build & Run (Windows)

```
just run aarch64                          # full build + boot
just debug aarch64                        # QEMU with -s -S, waits for GDB on :1234
```

Fast kernel-only iteration (embedded initramfs/minixfs are stale unless regenerated):

```
cargo build -p kernel-boot --bin kernel-boot-aarch64 --target aarch64-unknown-minix.json \
  --features "embed_initramfs,embed_minixfs,aarch64" -Zunstable-options -Zjson-target-spec \
  -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem --release
```

This embeds the existing `target/initramfs_data.rs`/`minixfs_data.rs`. The embedded binaries are
**only** current if the last `just run aarch64`/`just build aarch64` succeeded. To verify, extract
the embedded initramfs and compare (see "Verify embedded binaries" below).

## Attaching LLDB (the pattern that works on this Windows toolchain)

Backgrounding QEMU is unreliable (process dies when the shell exits). Instead, run two **parallel**
terminal calls: one runs QEMU with `-s` (gdb stub on :1234, boots normally), the other sleeps then
attaches LLDB in batch mode:

```
# Call A (blocks; user may need to keep it running):
qemu-system-aarch64 -machine virt -cpu cortex-a57 -m 256M -display none -serial null \
  -no-reboot -s -kernel target/aarch64-unknown-minix/release/kernel-boot-aarch64

# Call B:
sleep 22; timeout 60 lldb --batch -s target/hang.lldb \
  target/aarch64-unknown-minix/release/kernel-boot-aarch64
```

`target/hang.lldb` example:

```
gdb-remote 127.0.0.1:1234
register read pc elr_el1 spsr_el1 esr_el1 far_el1 sp_el0 ttbr0_el1 x0 x1 x8
quit
```

Stopping the QEMU call kills the probe; restart both for the next probe. The user can stop the
QEMU side while the LLDB side completes — the target stays stopped after `gdb-remote`.

## Identifying the Running Process

The kernel stores the current Proc pointer in `BOOT_CPU_STORAGE.current_proc` (offset 0). Proc
table entries live in `PROC_TABLE_ALIGNED`. Find addresses with:

```
rust-nm -n target/aarch64-unknown-minix/release/kernel-boot-aarch64 | grep -i "BOOT_CPU_STORAGE\|PROC_TABLE_ALIGNED"
```

Proc field offsets (AArch64; p_seg asserted at 288 in `crates/kernel/src/proc.rs`):
p_reg @ 0 (288B; ELR_EL1 @ 256, SPSR_EL1 @ 264), p_cr3 @ 288, p_nr @ 320,
p_rts_flags @ 336 (u32), p_misc_flags @ 340 (u32), p_caller_q @ 480,
p_getfrom_e @ 496 (i32), p_sendto_e @ 500 (i32), p_pending @ 512 (u128, 16-aligned!),
p_name @ 528 (16B), p_endpoint @ 544 (i32), p_sendmsg @ 548 (64B),
p_delivermsg @ 612 (64B), p_delivermsg_vir @ 676.

**Verify offsets against `crates/kernel/src/proc.rs` before probing.** The u128 `p_pending`
field forces 16-byte alignment, pushing `p_name` to 528 and `p_endpoint` to 544 — earlier
probes used 520/536 and read garbage. `proc_addr(n) = PROC_TABLE_ALIGNED + (NR_TASKS(5) + n) * 0x360`.

In LLDB (note: `expr printf` output is garbled in batch mode — prefer `memory read` with
precomputed absolute addresses):

```
expr -l c++ -- unsigned long long $proc = *(unsigned long long*)0x4008e850
memory read -s 4 -c 1 -f x $proc+320      # p_nr
memory read -s 4 -c 1 -f x $proc+336      # p_rts_flags (0 = runnable)
memory read -s 8 -c 1 -f x $proc+288      # p_cr3 (should equal TTBR0_EL1)
```

`memory read $proc+320` DOES evaluate the offset correctly (do not pre-subtract).

## Tracing a syscall chain with kernel_call breakpoints

When an IPC chain hangs (e.g., fork), break on the kernel's syscall handlers and read the
arguments. Kernel handler addresses shift on every rebuild — re-derive with `rust-nm`:

```
rust-nm -n target/aarch64-unknown-minix/release/kernel-boot-aarch64 | \
  grep -iE "sys_kernel_call_handler|do_fork_handler|sys_ipc_sendrec_handler"
```

Batch script (`target/trace.lldb`), each `c` resumes to the next hit:

```
gdb-remote 127.0.0.1:1234
b -a 0x4000db4c    # sys_kernel_call_handler (re-derive!)
b -a 0x40004498    # do_fork_handler
c
register read pc x0 x1
memory read -s 8 -c 6 -f x $x1    # the args array
memory read -s 4 -c 16 -f x <msg_user_va>   # user message buffer (from args[1])
c
... (repeat)
```

At `sys_kernel_call_handler` entry the register meanings are NOT the syscall args:

- **x0 = caller Proc pointer** (identify WHO: compare against `PROC_TABLE_ALIGNED` slots).
- **x1 = pointer to the args array `[call_nr, msg_ptr, ...]` on the kernel stack** —
  `memory read -s 8 -c 6 -f x $x1` yields `[call_nr, msg_ptr, 0, 0, 0, 0]`.
- `args[1]` (msg_ptr) is a **user VA**; while stopped in kernel mode the current process's
  TTBR0 is loaded, so `memory read` can read it directly (e.g. `0x3fcffce0`).
- For `SYS_VM_PAGING` (call 62) the subcommand is at msg bytes 8-11: `7` = VM_PAGING_FORK,
  `5` = VM_PAGING_QUERY_PROC, `2` = VM_PAGING_FREE.

Attach BEFORE the event you want to trace (sleep ~8.5s for a boot that reaches the shell at
~9s, with `/bin/echo 123` typed at 9s). Give **QEMU a longer timeout than the LLDB session**
so the trace doesn't die mid-chain; the last hits before the connection drops are the tail
of the failing chain. This traced the fork failure to `VM_PAGING_FORK -> hal::vm_paging_fork`
returning -1, with `sys_fork` (call 0) never reached.

## QMP state probe (post-mortem of a hang)

QEMU's QMP `xp` reads physical memory while the guest runs (no breakpoints needed):

```
python3 tools/qmp_state.py   # scans ALL slots (0..262), skips SLOT_FREE, decodes flags
```

Key reads: `current_proc` @ `BOOT_CPU_STORAGE+0`; `run_q_head` @ `+40` (16 pointers;
all null = no runnable process = kernel idle loop); each slot's rts/misc/getfrom/sendto/ELR.
RtsFlags: SENDING=0x4, RECEIVING=0x8, VMINHIBIT=0x200, NO_QUANTUM=0x8000, SLOT_FREE=0x1.
MiscFlags: REPLY_PEND=0x1, DELIVERMSG=0x40.

Caveats that produced wrong conclusions this session:

- **Re-derive `PROC_TABLE_ALIGNED`/`BOOT_CPU_STORAGE` from the current binary** — they move
  between builds. An old probe base (0x400576c0 vs real 0x40057700) made every slot read
  garbage ("no child slot exists" was wrong).
- **Scan ALL 262 slots**, not just 0..42 — the fork child can land anywhere.
- **`p_delivermsg` is a STALE buffer**: it holds the *last* message received, not the reason
  the process is currently blocked. A message sitting there + RECEIVING set does NOT mean the
  process is stuck waiting for it. The authoritative block state is rts_flags +
  p_getfrom_e/p_sendto_e.
- **DELIVERMSG set + RECEIVING set + message in p_delivermsg = lost wakeup**: the message was
  copied into p_delivermsg but never delivered to the user buffer nor enqueued (e.g., MFS in
  the mount hang).

## Fork-hang signatures

- All servers RECEIVING, run queues empty, current_proc stale = the kernel is idling in the
  `wfi` loop; nobody is runnable. This is a deadlock/lost-wakeup, not a crash.
- Child stuck in `waitpid` (sh: rts=RECEIVING, getfrom=PM, REPLY_PEND) while **no child slot
  exists anywhere** in the proc table = the fork "succeeded" on paper but no kernel Proc was
  ever created. Trace the VM_FORK chain: PM's `handle_fork` only checks the SENDREC *syscall*
  return (the source endpoint, always >= 0), never the VM reply's `m_type` — so a failed
  VM_FORK (garbage child endpoint) still replies a pid to the parent, which then waits forever.
  When a server's IPC "succeeds" but the payload is garbage, check whether it validates the
  reply type.

## QEMU `-d int` Trace Analysis

```
qemu-system-aarch64 ... -s -d int -D target/int_trace.txt -kernel <elf>
```

- `Taking exception 2 [SVC]` = svc syscall; `ESR 0x15/0x56000000` = SVC64.
- `with ELR 0x...` is the syscall address; `Exception return ... PC 0x...` is ELR+4
  (the kernel advances ELR in `el0_svc_handler` before the post-syscall hook).
- A **livelock signature**: the same SVC ELR repeating forever with `return PC = ELR+4`,
  never followed by a context switch (`return PC 0x1000000` = a fresh process entry).
- `return PC 0x1000000` between syscalls = a context switch to a new process.

The trace grows huge (~100+ MB/min during a livelock). Stop it once the pattern is visible.

## Known AArch64 Gotchas (this project)

1. **`kernel-boot/src/lib.rs` serial_write/serial_putc were no-ops on AArch64** (only x86_64 and
   riscv64 branches existed). `boot_init::load_and_prepare_proc`'s per-process messages
   (`/sbin/ds: ELF64 entry=...`) were invisible. AArch64 needs the PL011 branch
   (UART_DR=0x09000000, UART_FR=+0x18, FR_TXFF=1<<5).

2. **Executed instruction stream can differ from RAM** (VIPT I-cache / stale translation).
   Symptom: the `-d int` trace shows `svc` at ELR=0x1001ea8 while RAM at that VA decodes to
   `ldr` — and the actual `svc` (0xd4000001) sits 4 bytes earlier. When the executed bytes and
   the on-disk binary disagree, verify the embedded initramfs matches the on-disk binaries before
   suspecting the kernel (see below).

3. **Verify embedded binaries match disk binaries**:

   ```
   llvm-objcopy --dump-section .initramfs=target/embedded_initramfs.cpio \
     target/aarch64-unknown-minix/release/kernel-boot-aarch64
   # parse newc cpio (magic 070701; 110-byte header; 13 x 8-hex fields) and extract the binary
   cmp <extracted> target/aarch64-unknown-minix/release/pm
   ```

4. **Server receive-loop livelock**: servers loop `receive(ANY, &msg)` and retry when the syscall
   returns negative (`tbnz w0, #31, retry` around `svc`). If RECEIVE returns an error instead of
   blocking, the server spins in an svc storm. `mini_receive` in `crates/kernel/src/ipc.rs` only
   blocks (sets RECEIVING + dequeues) when `caller_rts & SENDING == 0 || has_reply_pend`;
   a process with SENDING set and no REPLY_PEND stays runnable → infinite receive loop.

5. **Boot messages**: AArch64 should print the x86/RISC-V sequence ("initializing allocator...",
   "allocator ready", "Hello MINIX/AArch64!", "  initializing boot processes...",
   "  loading boot processes...", per-proc lines, "  RAM disk mapped for MFS",
   "  enqueuing processes...", "  scheduler starting...", "  switching to userspace...").
   If per-process lines are missing, check the lib.rs serial_write cfg (gotcha 1).

6. **`mkboot`/QEMU process lock (LNK1104)**: `target/mkboot(.exe)` is locked while a
   build/QEMU process runs. `taskkill -F -IM qemu-system-aarch64.exe; taskkill -F -IM qemu-system-x86_64.exe`
   then re-run. There is no separate `jsh` anymore — Just uses the default shell.

7. **AArch64 boot proc page tables** (boot_init.rs): non-leaf links must be `PTE_TABLE` (0b11),
   not `pte_present()` (0b01 = block descriptor). The entry-0 PMD block (exception vectors)
   must stay AP=EL1-only — setting it EL0_RW causes a QEMU Cortex-A57 prefetch abort.

8. **Unimplemented HAL stubs silently fail kernel calls**: `hal::vm_paging_fork` was
   `-1 // Not implemented yet`, so every VM fork failed at `VM_PAGING_FORK` with child_cr3=0
   and the system hung with no child ever created. Before suspecting IPC/scheduling, grep
   `crates/arch-aarch64/src/hal.rs` for `Not implemented`/`TODO` — a kernel call can "fail"
   quietly at the arch layer while every shared layer looks correct.

9. **Per-process page tables have two structures**: boot procs get PUD[1] as a PMD table of
   2MB blocks (entry 0 = EL1-only exception vectors, rest EL0_RW); exec'd processes get PUD[1]
   as a 1GB block (AP=EL1-only) and PUD[0] as a low-GB PMD table (device MMIO 0x08000000-
   0x10000000 identity-mapped, RAM alias otherwise). Generic 4-level walks must handle both:
   block entries are shared/shared-with-fork-verbatim, table entries are deep-copied.
