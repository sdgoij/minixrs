# TIMER — x86 preemptive scheduling status report

Date: 2026-08-03. HEAD: `8aa61870`. Working tree: **uncommitted fixes** (operator
controls staging) — §3 ISR rewrite, §4 regressions re-applied, §5 root cause fixed.
Verified: timer ON, boot → shell, `echo done` 10/10, `ls /bin | cat` works, QEMU
integration + boot tests + host `cargo test` + clippy all green.

Goal (user constraint): **all three archs (x86, RISC-V, AArch64) reach a working
shell with preemptive scheduling enabled, matching original MINIX 3.3.0 as close
as practically possible.** The current "workaround" of leaving the PIT timer
effectively dead is not acceptable.

---

## 1. Status summary

| Item | State |
|---|---|
| Timer IRQ | **ON and firing at 100 Hz** — `restore()` unmasks IRQ0 (`and al, 0xfe`, asm.rs L956) and `eoi()`'s PIC fallback (G1) keeps the PIT alive |
| x86 timer ISR | **Fixed** — rewritten as a proper context-switch point (§3); boots to a working shell with the timer ON |
| Root cause of the shell I/O stall | **Confirmed** — `ipc_status_add_call` wrote IPC call numbers into `p_misc_flags` low bits, setting VIRT_TIMER/PROF_TIMER on receivers; the live timer's `vtimer_check` then caused spurious SIGVTALRM/SIGPROF on servers (mfs frozen on a phantom signal) deadlocking init's execve (see §5) |
| Fix (working tree, unstaged) | §3 ISR rewrite + §4 regressions (proc_no_time, ipc_status no-op, do_endksig re-enqueue) + G3 DELIVERMSG + G4 (remove_from_queue before PREEMPTED re-enqueue) + G11 tests — **all verified**: `echo done` 10/10 boots with the timer ON, pipelines/`ls` work, QEMU integration + boot tests pass |
| Remaining blocker | None for Phases 1-3. Known input-path limitation: >16-byte single piped bursts on x86 can lose bytes (UART 16-byte FIFO; drains added on the timer tick and every syscall entry improved but did not fully fix it — see §5) |
| RISC-V / AArch64 | **Now with real quantum accounting** (Phase 3, G6-G9): timer callbacks run `timer_int_handler`, `read_tsc_ctr_switch` returns rdtime/cntpct_el0; both boot to a shell, run pipelines and >16-byte bursts with the timer on (see §6) |

---

## 2. Root cause (confirmed)

`crates/arch-x86_64/src/apic.rs` `timer_isr_entry` (and `serial_isr_entry`), user-mode
return path:

```asm
// after all 15 GPRs were popped back:
"pop    rcx",      // RIP        → clobbers user RCX
"pop    rax",      // CS         → clobbers user RAX with 0x001B (=27, GUDATA_SEL|RPL3)
"pop    r11",      // RFLAGS     → clobbers user R11
"pop    r10",      // old_RSP    → clobbers user R10
```

The ISR restores the 15 GPRs and then **pops the CPU interrupt frame into the
user's registers**. Because CS = `0x001B` = 27, a timer tick landing right after a
syscall return turns RAX (the syscall return value / sender endpoint) into **27**.
This explains the bogus `vfs→27` / `mfs→27` sends and the varied race failures
(VM livelock, "memory/init rts=0 stranded") seen with the timer on. A timer tick
landing in the same window also destroys user RCX (return RIP) and R10/R11.

---

## 3. The fix (applied and verified in the working tree)

**Approach**: make the timer/serial ISR user-mode path a proper context-switch
point, matching C MINIX `apic_hwint` (`SAVE_PROCESS_CTX → handler → switch_to_user`,
see `.refs/minix-3.3.0/minix/kernel/arch/i386/apic_asm.S` L17-35 and
`proc.c::switch_to_user` L234-409) and the working RISC-V/AArch64 timer callbacks:

1. Save the interrupted user context (15 GPRs + frame RIP/RFLAGS/userRSP/SS) into
   the running process's `p_reg`.
2. `pick_proc_raw()` → next process (or the current one if nothing else).
3. `set_cpulocal_proc_asm(next)` then `restore(next)` — never iretq from the frame
   directly. `restore()` (asm.rs L904) already loads CR3, builds the iretq frame from
   `p_reg`, loads all GPRs (r15 last), masks/unmasks IRQ0, and iretq's to user mode.

This mirrors `exception_page_fault_entry`'s handled path (asm.rs L613-624:
`add rsp,176; call pick_proc_raw; call set_cpulocal_proc_asm; call restore`) and the
RISC-V callback (`kernel-boot/src/riscv64.rs` L444-518).

### 3.1 New helper — `save_timer_context`

Add to `crates/kernel-boot/src/main.rs` right after `save_fault_context` (L630-666):

```rust
#[unsafe(no_mangle)]
pub unsafe extern "C" fn save_timer_context(frame: *const u64) -> *mut core::ffi::c_void
```

- Reads the current proc via `arch_x86_64::cpulocals::get_cpulocal_proc_ptr()`.
- Copies from the ISR frame into `p_reg` (offsets matching `restore()`: 0=rax, 8=rbx,
  16=rcx, 24=rdx, 32=rsi, 40=rdi, 48=r8, 56=r9, 64=r10, 72=r11, 80=r12, 88=r13,
  96=r14, 104=r15, 112=rbp, 160=rip, 168=rsp, 176=rflags).
- **Returns the current proc pointer** so the asm can fall back to resuming it when
  `pick_proc_raw()` returns null (RISC-V/AArch64 do the same: if `pick_proc()` is
  None, return to the interrupted process).

Timer ISR frame layout (15 GPRs pushed in order rax,rcx,rdx,rbx,rbp,rsi,rdi,r8,r9,
r10,r11,r12,r13,r14,r15, then the CPU's ring-3 40-byte frame — no error code):

```
frame[0]=r15 [1]=r14 [2]=r13 [3]=r12 [4]=r11 [5]=r10 [6]=r9 [7]=r8
frame[8]=rdi [9]=rsi [10]=rbp [11]=rbx [12]=rdx [13]=rcx [14]=rax
frame[15]=RIP [16]=CS [17]=RFLAGS [18]=user RSP [19]=SS
```

p_reg writes: rax←[14], rbx←[11], rcx←[13], rdx←[12], rsi←[9], rdi←[8],
r8←[7], r9←[6], r10←[5], r11←[4], r12←[3], r13←[2], r14←[1], r15←[0],
rbp←[10], rip←[15], rsp←[18], rflags←[17]. (rcx/r11 hold the user's real regs,
like `save_fault_context` — the dedicated 160/176 slots carry RIP/RFLAGS.)

### 3.2 Rewrite `timer_isr_entry` (apic.rs L891)

```asm
push rax..r15                  ; 15 GPRs (120 bytes)
call timer_isr_c_handler       ; handler + EOI
; check CS.RPL with GPRs still pushed — CS is at [rsp+128]
mov  rdx, [rsp+128]
and  rdx, 3
cmp  rdx, 0
je   kernel_path

; user mode — context-switch point:
call save_timer_context        ; rax = current proc (aligned at RSP0-160, like the existing call)
add  rsp, 160                  ; discard 15 GPRs + 40-byte CPU frame → RSP0
test rax, rax
jz   halt                      ; no current proc (shouldn't happen)
mov  r15, rax                  ; r15 = current (callee-saved)
call pick_proc_raw             ; rax = next runnable or null (call at RSP0)
mov  rdi, r15
test rax, rax
cmovnz rdi, rax                ; rdi = next if runnable, else current
call set_cpulocal_proc_asm
mov  rdi, rax
call restore                   ; never returns
halt: cli; hlt; jmp halt

kernel_path:
pop r15..rax                   ; restore the 15 GPRs
and  qword ptr [rsp+16], 0xfffffffffffffdff   ; clear IF in saved RFLAGS
pop  rcx                       ; RIP
add  rsp, 8                    ; skip CS
pop  r11                       ; RFLAGS (IF cleared)
add  rsp, 16                   ; skip old_RSP/old_SS (QEMU TCG 40-byte same-ring frame)
push r11; popfq
push rcx; ret
```

Notes:
- **Stack alignment**: after 15 pushes the call site is at `RSP0-160`. The existing
  code calls `timer_isr_c_handler` at the same position and works, so RSP0 is
  16-aligned in practice; do NOT add a `sub rsp,8` (the page-fault entry needs one
  because its CPU frame is 48 bytes, the timer's is 40).
- `restore()`, `pick_proc_raw`, `set_cpulocal_proc_asm`, `save_timer_context` are all
  `#[unsafe(no_mangle)]`; the ISR entries must be `#[cfg(target_os = "none")]` so the
  host test build doesn't try to link the cross-crate symbols.
- The kernel-mode path is unchanged in behavior (QEMU TCG same-ring 40-byte frame:
  skip old_RSP/old_SS).

### 3.3 Mirror in `serial_isr_entry` (apic.rs L1017)

Same structure; push all 15 GPRs (currently only 9), call the registered handler,
EOI master PIC (`out 0x20, al`), then the identical user/kernel paths. Every
interrupt is a context-switch point in C MINIX too (`apic_hwint`).

### 3.4 Verification (timer ON, current tree)

- Boot reaches the shell prompt, `echo done` prints `done` **10/10** with the
  timer unmasked (`restore()` `and al, 0xfe`).
- Pipelines (`echo hello | cat`), `ls /bin`, and ≤16-byte input bursts are
  byte-exact.
- The §5 deadlock (mfs frozen on a phantom SIGVTALRM) is gone; the run queue
  stays clean (verified by QMP probe with fresh symbol addresses).

---

## 4. Regressions previously fixed (re-applied in the working tree)

These were working-tree fixes during earlier timer sessions; the reset to main
removed them, and this session re-applied them (all verified):

1. **`proc_no_time` divergence** (crates/kernel/src/sched.rs L362): Rust version used
   to set `RTS_PREEMPTED` + dequeue for kernel-scheduled processes; C
   (`proc.c` L1826) only renews the quantum. The Rust version stranded processes.
   **Re-applied: renew-only.**
2. **`ipc_status_add_call` clobbering `p_misc_flags`** (crates/kernel/src/ipc.rs):
   wrote the IPC call into misc_flags low bits which overlap MF_REPLY_PEND /
   MF_VIRT_TIMER / MF_PROF_TIMER → spurious SIGVTALRM/SIGPROF. C writes it to
   `p_reg.IPC_STATUS_REG`; this port doesn't store IPC status at all. **Re-applied:
   `ipc_status_add_call`/`add_flags`/`clear` are no-ops — this was the root cause
   of the §5 shell-I/O stall.**
3. **`do_endksig_handler` didn't re-enqueue** (system.rs): cleared RTS_SIG_PENDING
   without the enqueue that C's `RTS_UNSET` implies. **Re-applied.**
4. **PIC EOI fallback** in `eoi()` (apic.rs): write master PIC EOI when APIC is
   disabled, else the PIC ISR bit stays set and blocks IRQ0 and serial IRQ4.
5. **40-byte same-ring interrupt frames** (QEMU TCG): `add rsp,16` after the RFLAGS
   pop in the kernel-mode ISR return paths.

---

## 5. Remaining blocker — shell I/O stall under preemption (RESOLVED)

### What was observed (frozen, with the timer ON)

Boot reached `init: starting shell...`; the `#` prompt appeared but `echo done`
never executed. Frozen state: all servers blocked on RECEIVE, the run queue
empty, CPU in the syscall-handler idle loop (`sti; hlt; cli; pick_proc`), and
the shell's execve chain deadlocked:

```
init  → SENDREC(PM, PM_EXEC_NEW)          ; getfrom=0 (pm), REPLY_PEND
pm    → SENDREC(VFS)                      ; getfrom=1 (vfs)
vfs   → SENDREC(MFS)                      ; sendto=7, waiting for the reply
mfs   → rts=0x30 SIGNALED|SIG_PENDING     ; frozen on a PHANTOM SIGNAL
```

mfs had `p_pending` bit 26 (SIGVTALRM) set and was blocked on signal
processing, so vfs's read reply could not be delivered; the whole exec chain
stalled.

### Root cause (confirmed)

`mini_send`'s direct-delivery path calls `ipc_status_add_call(dst, call)`,
which stored the IPC call number (SEND=46, NOTIFY=45, …) into the receiver's
`p_misc_flags` low 6 bits. Those bits are **real flags** in this port:

| bit | flag |
|---|---|
| 0x1 | REPLY_PEND |
| 0x2 | VIRT_TIMER |
| 0x4 | PROF_TIMER |
| 0x8 | KCALL_RESUME |

So a SEND delivery set VIRT_TIMER|PROF_TIMER|KCALL_RESUME on the receiver.
With the timer dead (HEAD workaround) nothing noticed. With the timer alive
(G1), `timer_int_handler` → `vtimer_check` runs every tick and saw the
phantom VIRT_TIMER with `p_virt_left == 0` → `cause_sig(SIGVTALRM)` → the
server froze on SIGNALED|SIG_PENDING. C MINIX stores IPC status in
`p_reg.IPC_STATUS_REG` (a real register slot); this port has no such slot, so
writing it to misc_flags was a porting bug.

### The fix (working tree)

- `crates/kernel/src/ipc.rs`: `ipc_status_add_call` / `ipc_status_add_flags` /
  `ipc_status_clear` are now **no-ops** (no IPC_STATUS_REG in this port).
- `crates/kernel/src/system.rs`: `do_endksig_handler` now re-enqueues the
  target when clearing SIG_PENDING makes it runnable (C's `RTS_UNSET` side
  effect) — §4 item 3, also lost in the reset.

### Verification

- Timer ON: boot reaches the shell, `echo done` prints `done` **10/10**.
- Pipelines work (`echo hello | cat` → `hello`); `ls /bin` lists all bins;
  single commands and ≤16-byte input bursts are byte-exact.
- `just test-qemu-x86` (integration) and `just test-boot-x86` (boot tests)
  both pass; host `cargo test` 37 suites green; `cargo clippy -- -D warnings`
  clean; RISC-V cross-check compiles.

### Known input-path limitation (pre-existing, improved this session)

x86 >16-byte single piped bursts can still lose bytes: the UART has a 16-byte FIFO and
`ser_input::read_blocking` spins with IF=0 (x86 `cpu_idle()` is `pause()`; the syscall's
SF_MASK keeps IF=0), so the serial ISR cannot drain mid-burst. This session added two
drains: on the 100 Hz timer tick and on **every syscall entry** (`drain_uart_input`,
kernel-boot main.rs) — matching AArch64's "poll the UART on every IRQ" pattern for a
kernel that runs syscalls with IF=0. Capture went from ~26/39 to ~31-34/39 bytes (racy).

Deliberately NOT done: `cpu_idle()` = `sti; hlt; cli` — it lets the timer fire in kernel
mode and burn the waiting process's quantum (`context_stop` runs per tick in this port,
unlike C MINIX which accounts at context-switch time), causing NO_QUANTUM churn and
worse losses. RISC-V/AArch64 are not FIFO-limited (SBI console reads / PL011 32-byte
FIFO + every-IRQ drain).

---

## 6. Arch comparison — why RISC-V/AArch64 work

### 6.1 Are the timers actually running? (current tree)

**Yes — all three. x86's timer was dead at HEAD (missing PIC EOI fallback); with G1
applied it fires continuously at 100 Hz, and its ISR is a real context-switch point.**

| | x86 (current tree) | RISC-V | AArch64 |
|---|---|---|---|
| Timer source | PIT ch0 `init_pit(100)` (apic.rs) | CLINT `init_timer(100)` (clint.rs L46) | CNTP generic timer `init_timer()` (timer.rs L21) |
| IRQ unmasked? | `restore()` unmasks IRQ0 (`and al,0xfe`, asm.rs L956) | `sie.STIE` set (riscv64.rs L552-564) | GIC PPI 30 enabled + `daifclr #2` (aarch64.rs L640-703) |
| Per-tick ack | PIC EOI fallback in `eoi()` (apic.rs L318) | `handle_timer_interrupt` reprograms stimecmp (clint.rs L68) | `timer_irq_ack` reprograms CNTP (timer.rs L38) |
| **Actually firing?** | **YES — 100 Hz periodic** | **YES — 100 Hz periodic** | **YES — 100 Hz periodic** |
| Callback | `timer_isr_entry` (save→pick→switch, apic.rs L891) | `riscv_timer_callback` (save→pick→switch, riscv64.rs L444) | `aarch64_timer_callback` (save→pick→switch, aarch64.rs L370) |
| DELIVERMSG for picked proc | yes (`isr_deliver_msg`, kernel-boot main.rs L746) | yes (riscv64.rs L479-495) | yes (aarch64.rs L398-431) |
| Quantum accounting in ISR | **YES** — `timer_int_handler` → `context_stop` → `proc_no_time` → `notify_scheduler` | **YES** (Phase 3: `riscv_timer_callback` → `timer_int_handler`; `read_tsc_ctr_switch` = rdtime) | **YES** (Phase 3: `aarch64_timer_callback` → `timer_int_handler`; `read_tsc_ctr_switch` = cntpct_el0) |
| Timer can dequeue current | yes (NO_QUANTUM + notify_scheduler) | yes (same path, proven by Phase 3) | yes (same path, proven by Phase 3) |

RISC-V routing: `trap_handler` `SUP_TIMER_INTR` → `handle_timer_interrupt`
(reprogram stimecmp) + UART drain + timer callback (trap.rs L177-189).
AArch64 routing: `el1_irq_handler_c` → `timer_irq_ack` + timer callback
(exception.rs L259). x86 routing: `timer_isr_entry` (naked asm) →
`timer_isr_c_handler` → `timer_int_handler` + `eoi()`.

### 6.2 Consequences

- **At HEAD the x86 timer was the only dead one** (missing PIC EOI fallback); G1 fixed
  it and the PIC EOI also unblocked serial IRQ4. RISC-V/AArch64 always ran real
  periodic preemption.
- **RISC-V/AArch64 preemption = "pick a different runnable process" only.** Their
  callbacks never dequeue the running process (no quantum accounting), so the timer
  can't strand anyone; `pick_proc` may simply return the current process again.
- **x86 does full MINIX-style quantum accounting** (`proc_no_time` → `notify_scheduler`
  → NO_QUANTUM dequeue → sched re-enqueues via SYS_SCHEDULE). The path is now
  exercised and works under the live timer (boot + pipelines + burst input). G4
  (PREEMPTED double-link) still needs explicit verification.
- C MINIX *does* do quantum accounting (`proc.c` `proc_no_time` L1826), so x86
  matches C; RISC-V/AArch64 effectively never preempt via the timer.

### 6.3 Per-arch timer details

| | x86 | RISC-V | AArch64 |
|---|---|---|---|
| Timer handler | `timer_int_handler` → `context_stop` → quantum accounting | `timer_int_handler` → `context_stop` → quantum accounting | `timer_int_handler` → `context_stop` → quantum accounting |
| `read_tsc_ctr_switch` | real TSC (hal.rs L228) | real `rdtime` (hal.rs L868, clint.rs) | real `cntpct_el0` (hal.rs L187) |
| Timer can dequeue current | **yes** (proc_no_time → notify_scheduler) | **yes** (proven by Phase 3) | **yes** (proven by Phase 3) |
| Preemption style | timer-driven quantum expiry | timer-driven quantum expiry (was cooperative) | timer-driven quantum expiry (was cooperative) |
| Save+pick+restore in callback | yes (`timer_isr_entry` + `save_timer_context`) | yes (riscv64.rs L444) | yes (aarch64.rs L370) |
| DELIVERMSG for next proc | yes (`isr_deliver_msg`) | yes (L479-495) | yes (L398-431) |

Only x86 previously did TSC-based quantum accounting in the timer ISR; **Phase 3 rolled the same
path out to RISC-V and AArch64** (`read_tsc_ctr_switch` now returns rdtime / cntpct_el0, the
timer callbacks run `timer_int_handler` in both modes, and both boot to a shell with pipelines
and >16-byte bursts). All three archs can now NO_QUANTUM-dequeue the running process from the
timer and rely on the sched reply path (`SCHEDULING_NO_QUANTUM` → `sys_schedule`) to re-enqueue.

Boot procs are PREEMPTIBLE (`SYS_PROC | PREEMPTIBLE`, table.rs `proc_init`) and
have the SCHED server as user scheduler, so on x86 quantum expiry triggers
`notify_scheduler`. The SCHED server **does** handle `SCHEDULING_NO_QUANTUM`
(`do_noquantum` → `schedule_process_local` → `sys_schedule`, servers/src/sched.rs
L195, L461-585), so the re-enqueue path exists.

---

## 7. Code reference points

- `crates/arch-x86_64/src/apic.rs`: `timer_isr_entry` L891 (context-switch point:
  save→pick→DELIVERMSG→restore), `serial_isr_entry` L1017 (same), `timer_isr_c_handler`
  L861, `eoi` L318 (PIC EOI fallback), `init_pit` L555.
- `crates/arch-x86_64/src/asm.rs`: `restore` L904 (unmasks IRQ0 L956),
  `set_cpulocal_proc_asm` L529, `exception_page_fault_entry` L549 (the pattern
  mirrored), `syscall_entry` L1021.
- `crates/kernel-boot/src/main.rs`: `save_timer_context` L699, `isr_deliver_msg` L746,
  `save_fault_context` L630, `syscall_handler_c` L762, `kmain_body` L163-240 (PIT/
  serial setup, `mask_timer_irq` L172).
- `crates/kernel/src/sched.rs`: `proc_no_time` L362 (renew-only for kernel-scheduled),
  `notify_scheduler` L302, `pick_proc` L253, `pick_proc_raw` L288,
  `enqueue`/`enqueue_head`/`dequeue`.
- `crates/kernel/src/clock.rs`: `timer_int_handler` L304, `context_stop` L428
  (quantum decrement + proc_no_time), `vtimer_check` L231 (spurious-signal trigger),
  `ms_2_cpu_time` L393.
- `crates/kernel/src/ipc.rs`: `ipc_status_add_call` L66 (no-op — the §5 phantom-signal
  root cause), `ipc_status_add_flags` L82 / `ipc_status_clear` L97 (no-ops),
  `mini_send` L145, `mini_receive` L300, `delivermsg`.
- `crates/kernel/src/system.rs`: `do_endksig_handler` L3837 (re-enqueue on SIG_PENDING
  clear), `cause_sig` L1495, `send_sig` L1459, `sched_proc` L1684.
- `crates/arch-riscv64/src/hal.rs` L868 / `crates/arch-aarch64/src/hal.rs` L187:
  `read_tsc_ctr_switch` returns 0.
- `crates/kernel-boot/src/riscv64.rs` L444-518 and `crates/kernel-boot/src/aarch64.rs`
  L370-434: the working timer callbacks (save → pick → DELIVERMSG → switch).
- `crates/minix-rt/src/lib.rs`: `execve` L120, `brk` L1057, `sbrk` L1087.
- `crates/userland/src/lib.rs`: `init` L592 (`execve("/bin/sh")`).
- C references: `.refs/minix-3.3.0/minix/kernel/arch/i386/apic_asm.S` L17-35,
  `.refs/minix-3.3.0/minix/kernel/proc.c` L234-409 (`switch_to_user`), L1826-1836
  (`proc_no_time`).

---

## 8. Next steps

Phases 1-4 are **complete and verified**: G1-G11 all in; `echo done` 10/10 on x86,
pipelines + bursts on all three archs, integration/boot tests, `runqueues_ok` now
terminates on cycles (2-cycle + self-loop tests), clippy clean. Remaining (all minor):

1. **x86 >16-byte burst input** (§5): drains on the timer tick and every syscall entry
   improved capture but a single large piped burst can still lose bytes (UART 16-byte
   FIFO). A C-faithful fix (blocking TTY read with the kernel idling at IF=1) is out of
   scope; interactive-sized input is byte-exact.
2. **Full boot matrix as a standing gate**: re-run `echo done` 10/10 on x86 plus a
   boot+pipeline on RISC-V/AArch64 after any further change, and `cargo clippy --
   -D warnings` + `cargo test` before reporting back.

---

## 9. Session notes (context for the next session)

- The working-tree debug instrumentation used during this investigation (now gone
  with the reset): `S>` + m_type in `mini_send`, `DROP <why>` wakeup prints,
  `v`/`V` VM receive prints, `+I`/`-I` init enqueue/dequeue prints, `RESCH`,
  one-shot `IDLE cur=...` proc-table dump in the syscall handler's idle path, `EXL`
  exec-load print. These were added only to find the VM_BRK message; do not
  re-add them — probe with LLDB instead.
- With the timer masked the boot trace shows the healthy exec chain:
  `S>init 0000` (PM_EXEC) → `pm→vfs` → `vfs→mfs` → `EXL init` → `# echo done` → `done`.
- Endpoints (gen 0): pm=0, vfs=1, rs=2, memory=3, sched=4, tty=5, ds=6, mfs=7,
  **vm=8**, pfs=9, init=10, ramdisk=11 (arch-common/src/com.rs).

### 2026-08-03 session — shell I/O stall root-caused and fixed

- **Root cause**: `ipc_status_add_call` (mini_send direct delivery) wrote the IPC
  call number into `p_misc_flags` low bits, setting VIRT_TIMER/PROF_TIMER on the
  receiver. With the timer live, `vtimer_check` caused spurious SIGVTALRM/SIGPROF
  (mfs frozen on `SIGNALED|SIG_PENDING`, `p_pending` bit 26), deadlocking
  init→PM→VFS→MFS exec chain. Fix: `ipc_status_*` stores are no-ops (this port
  has no `IPC_STATUS_REG`); `do_endksig_handler` re-enqueues on SIG_PENDING clear.
  Verified: timer ON, `echo done` 10/10, pipelines + `ls` work, integration/boot
  tests + clippy green.
- **LLDB/QMP probe addresses move with every rebuild.** This session wasted
  cycles chasing a "run_q_head[0] = &head[0] corruption" that was a stale-address
  artifact: `CPU_LOCAL_VARS` moved 0x4a19d8 → 0x4a19d0 (+8) and MONOTONIC
  0x2537f8 → 0x253800 after the kernel relink. ALWAYS re-run `rust-nm` on the
  fresh `kernel-boot` and re-derive CPU_LOCAL_VARS (proc_ptr@+0, run_q_head@+0x420,
  run_q_tail@+0x4A0), PROC_TABLE (stride 848), ser_input READ/WRITE_IDX before
  probing. `tools/x86_probe2.py` and `tools/x86_ipcprobe.py` (both with the
  current addresses) are the reusable dumps.
- The frozen state is reproducible: boot with input, `READ=0..1 WRITE=10`,
  MONOTONIC≈1388, all servers RECEIVING, CPU in `syscall_handler_c` idle loop.
- Known input-path limit (not caused by the timer fix): >16-byte piped bursts
  lose bytes (UART 16-byte FIFO; x86 `read_blocking` spins with IF=0 and
  `cpu_idle()` = `pause()`).

### 2026-08-03 session 2 — G4, G11, burst input, Phase 3 (G6-G9)

- **G4**: x86 `syscall_handler_c` PREEMPTED handling now does `remove_from_queue`
  before `enqueue`/`enqueue_head` (syscall-return branch AND the pick-loop
  `is_preempted()` branch), matching RISC-V. pick_proc does not dequeue, so a
  preempted proc still linked in the queue would otherwise be linked twice.
- **G11**: `test_notify_scheduler_dequeues_runnable_proc` proves the
  `old==0 && !is_runnable()` dequeue fires; `test_preempted_re_enqueue_no_double_link`
  covers the G4 pattern. (374 host kernel tests.)
- **Burst input**: added `drain_uart_input()` (UART FIFO → ser_input ring) to the
  timer tick and to every syscall entry. `cpu_idle()` = `sti;hlt;cli` was tried and
  REVERTED (kernel-mode timer ticks burn the spinning process's quantum → churn).
  A 1-byte FIFO trigger was tried and REVERTED (garbled input). Result: ≤16-byte
  bursts byte-exact; >16-byte single bursts improved (~31-34/39) but racy.
- **Phase 3 (G6-G9)**: RISC-V/AArch64 `read_tsc_ctr_switch` now returns real
  rdtime/cntpct_el0 (AArch64 cpulocals gained `tsc_ctr_switch` accessors); their
  timer callbacks call `kernel::clock::timer_int_handler()` (full accounting in
  both modes, SPP/SPSR check only around save/pick/switch); both boot paths set
  `cpu_set_freq` (10 MHz / 62.5 MHz to match their counters). Both archs boot to a
  shell and run pipelines + >16-byte bursts with the timer on.
- **Probe addresses moved again after the drains rebuild**: ser_input indices
  0x28e840→0x28e940, MONOTONIC 0x253800→0x253900 (re-derive after every build —
  see the aarch64-debugging skill's stale-base trap section).
- **runqueues_ok hardened**: the tail-reachability and runnable walks are now bounded
  by NR_PROCS_TOTAL steps, so a cyclic queue (self-loop OR 2-cycle A→B→A) returns
  false instead of spinning forever. `test_runqueues_ok_detects_two_cycle` covers it.

---

## 10. Plan — timer/preemption parity across all three archs

**Principle**: every arch implements the same timer/preemption feature set,
matching C MINIX 3.3.0. RISC-V and AArch64 must gain quantum accounting (they
currently disable it via `read_tsc_ctr_switch() → 0`), and x86 must gain a live
timer with a correct context-switching ISR.

### 10.1 Target feature set (each arch)

1. **Live periodic timer** — 100 Hz interrupt that actually fires every tick.
2. **Per-tick accounting** — `context_stop(current)` (cycle-based quantum
   decrement, C `arch_clock.c` L205-307) + `timer_int_handler` (monotonic/
   realtime, virtual timers, load average, C `clock.c` L76-177) every tick.
3. **Timer ISR is a context-switch point** — save the interrupted user context
   into `p_reg`, `pick_proc()`, deliver any pending DELIVERMSG to the picked
   process, then restore it (C `apic_hwint` L17-35 → `switch_to_user`
   L234-409).
4. **Quantum expiry → `proc_no_time`** — kernel-scheduled: renew quantum;
   preemptible with user scheduler: `notify_scheduler` (NO_QUANTUM + dequeue +
   `SCHEDULING_NO_QUANTUM` to sched) → sched re-enqueues via `sys_schedule`
   (servers/src/sched.rs `do_noquantum` L195).
5. **Cooperative preemption at syscall return** — PREEMPTED handling
   consistent (remove_from_queue + re-enqueue if still runnable) and DELIVERMSG
   delivered to the resumed process.

### 10.2 Gap table

| # | Arch | Gap | Fix | Where / status |
|---|---|---|---|---|
| G1 | x86 | Timer dies after 1 tick — `eoi()` is a no-op when APIC disabled, PIC ISR bit for IRQ0 sticks (also blocked serial IRQ4) | PIC EOI fallback in `eoi()` (`out 0x20, PIC_EOI`) | **DONE** — arch-x86_64/src/apic.rs `eoi` L318 |
| G2 | x86 | Timer/serial ISR user-mode return clobbers RAX/RCX/R10/R11 | Save→pick→restore; add `save_timer_context`; `#[cfg(target_os="none")]` on the entries | **DONE** — apic.rs `timer_isr_entry` L891 / `serial_isr_entry` L1017; kernel-boot main.rs `save_timer_context` L699 |
| G3 | x86 | ISR switch path missing DELIVERMSG delivery for the picked process (RISC-V/AArch64 do it) | Mirror riscv64.rs L479-495 (deliver + write_retval + clear flag) before `restore(next)` | **DONE** — `isr_deliver_msg` (kernel-boot main.rs L746) + apic.rs asm |
| G4 | x86 | syscall PREEMPTED path: plain `enqueue` (kernel-boot main.rs `syscall_handler_c`); RISC-V does `remove_from_queue` + `enqueue` to avoid double-link | Match RISC-V; verify `remove_from_queue` is safe on a non-linked proc | **DONE** — `remove_from_queue` before the PREEMPTED re-enqueues (syscall branch + pick-loop branch); `test_preempted_re_enqueue_no_double_link` |
| G5 | x86 | Shell I/O stall / VM_BRK livelock at init's exec under preemption (§5) | **RESOLVED** — root cause: `ipc_status_add_call` set VIRT_TIMER/PROF_TIMER in misc_flags → spurious SIGVTALRM/SIGPROF froze mfs; fixed as no-op + `do_endksig` re-enqueue | ipc.rs, system.rs |
| G6 | RISC-V | No quantum accounting — `read_tsc_ctr_switch` returns 0 | Implement: store real `rdtime` in cpulocals `tsc_ctr_switch`, return it | **DONE** — arch-riscv64/src/hal.rs |
| G7 | RISC-V | Timer callback only updates clocks | Replace clock-only block with `kernel::clock::timer_int_handler()` (runs for kernel AND user mode, like x86); keep SPP check only around save/pick/switch | **DONE** — kernel-boot/src/riscv64.rs `riscv_timer_callback` (+ `cpu_set_freq` 10 MHz) |
| G8 | AArch64 | Same as G6 — returns 0 | Implement with `cntpct_el0` | **DONE** — arch-aarch64/src/hal.rs + cpulocals.rs accessors |
| G9 | AArch64 | Same as G7 — clocks only | Same as G7 | **DONE** — kernel-boot/src/aarch64.rs `aarch64_timer_callback` (+ `cpu_set_freq` 62.5 MHz) |
| G10 | shared | §4 fixes (`proc_no_time`, `ipc_status_*`, `do_endksig`, same-ring frame) were uncommitted and lost | Re-apply + tests | **DONE** — kernel/src/sched.rs, ipc.rs, system.rs (verified: `echo done` 10/10, integration/boot tests, clippy) |
| G11 | shared | `notify_scheduler` dequeue condition (sched.rs L302) — verify `old==0 && !is_runnable()` actually dequeues | Test `old==0 && !is_runnable()` actually dequeues | **DONE** — `test_notify_scheduler_dequeues_runnable_proc` |

### 10.3 Execution order

1. **Phase 1 — x86 timer alive + correct ISR (G1+G2)** — **DONE, verified**: 10× boot
   with the timer on, `echo done` 10/10.
2. **Phase 2 — x86 preemption under load (G3+G4, then G5)** — **DONE**: G3
   (`isr_deliver_msg`), G4 (`remove_from_queue` before PREEMPTED re-enqueues),
   G5 (phantom signals, §5). Gate `ls /bin | cat` + repeated pipelines pass;
   x86 >16-byte single bursts remain racy (see §5 — improved but not fully fixed).
3. **Phase 3 — quantum accounting on RISC-V/AArch64 (G6-G9)** — **DONE, verified**: both
   archs boot to a shell and run pipelines + >16-byte bursts with the timer on.
4. **Phase 4 — shared-code hardening (G11 + G4 tests)** — **DONE**: `notify_scheduler`
   dequeue test, PREEMPTED double-link test, `runqueues_ok` cycle detection (bounded
   walk — catches self-loops AND 2-cycles that previously hung the checker);
   `cargo clippy -- -D warnings` clean; host tests 37 suites green.

### 10.4 Risks / notes

- Phase 3 exercises `notify_scheduler` on RISC-V/AArch64 for the first time; any
  bug in the sched reply/re-enqueue path will surface on all archs. That is the
  point of parity, but Phase 2 must prove the path on x86 first.
- C MINIX accounts kernel-mode ticks to idle (`context_stop_idle`), not to the
  interrupted process. The Rust `timer_int_handler` calls `context_stop(current)`
  unconditionally; RISC-V/AArch64 currently skip kernel-mode ticks entirely. For
  parity, decide: run `timer_int_handler` in both modes (x86 behavior) and accept
  kernel ticks billing the interrupted process, or add the idle-accounting
  distinction. Recommend the former (simpler, matches x86, error is small).
- G1 landed: the timer fires continuously and serial IRQ4 is not re-blocked;
  the burst-input path works for ≤16-byte bursts (see §5 for the >16-byte limit).
- x86 `restore()` masks IRQ0 at entry and unmasks before iretq; with a live
  timer this window is already covered. No change expected, but re-verify.
