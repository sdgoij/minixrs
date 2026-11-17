# The exec deadlock (KNOWN_ISSUES item 50)

An `exec` that failed at random with `ELOCKED`, and the PM→VFS round-trip that
caused it.

## Symptom

`just test-interactive-bash-<arch>` intermittently failed with

```
sh: 'bash' not found (/bin/bash: -208)
```

on a `/bin/bash` that was in the image. `-208` is `ELOCKED` (`crates/kernel/src/ipc.rs`),
the kernel's answer when a blocking send would close a cycle. It is not the
`ENOMEM`/loader suspect the entry listed: the call never reached PM.

It was timing-dependent. Ten consecutive interactive-bash runs passed on a
quiet host, and the failure needed the typed command to land in the boot tail,
while `/bin/session` was starting the desktop.

## Instrument

No kernel or server prints. The chain came from LLDB over QEMU's gdbstub. Four
things had to be learned first, and each cost a run:

- **A software breakpoint written while QEMU is frozen at reset does not
  stick.** `-kernel <elf>` loads the kernel image as a read-only memory region,
  so writes at 0x200000 are dropped (reads work, which is what makes it look
  like the breakpoint is armed). Use a **hardware** breakpoint (`Z1`).
- **A DWARF kernel cannot be booted through `mkboot`.** With
  `CARGO_PROFILE_RELEASE_DEBUG=2`, `objcopy -O binary` carries the debug sections
  and the raw kernel balloons to 132 MB; embedded at 0x200000 it exhausts RAM and
  the boot aborts loading `/sbin/fb`. Build the debug ELF separately as LLDB's
  symbol file, and boot a `--strip-debug` binary from the same build.
- **Attach after boot, not at reset.** Stop the guest with the QEMU monitor
  (`-monitor tcp:...`) when the shell prompt appears, then connect LLDB. The CPU
  is in 64-bit paged mode (writes work) and the boot tail is preserved; the
  driver types the command at `session: started` after LLDB continues.
- **At the inlined `deadlock()` call site the arguments are optimized out of
  DWARF.** Read them from registers instead: at `ipc.rs:471`, `rdi` is
  `caller_ptr`, `esi` is `dst_e`, and `r8d` is the caller's endpoint. The chain
  itself is reproduced from the Proc table (`p_rts_flags`, `p_sendto_e`,
  `p_getfrom_e`, `p_endpoint`), which is what `deadlock()` walks.

The breakpoint fires only in `mini_send`'s blocking branch (the destination is not
receiving), which is exactly the deadlock candidate. A callback walks the chain
and stops only when it closes on the caller.

## The captured chain

On the failing run the caller was **PM** sending `user_mtype=2310` (`VFS_PM_EXEC`,
0x906) to **VFS** (endpoint 1). VFS was recorded as:

```
rts=0xc   (SENDING|RECEIVING)
mf=0x1001 (REPLY_PEND|FLUSH_TLB)
sendto=0, getfrom=0  (PM)
```

`blocked_on()` reads SENDING first, so VFS is seen blocked on PM, and PM is the
caller: the detector returns `ELOCKED`.

## The cause

`do_exec` forwarded `VFS_PM_EXEC` with a **blocking SENDREC**. A blocking SENDREC
sends, then receives. PM could send its exec request while VFS was still in the
send phase of a reply to an earlier PM request. Neither was then receiving, and
the kernel's `will_receive_sendrec` refuses to deliver to a process whose
`SENDING` bit is set. Both sides waited on a send: the cycle the detector names.

The reference does not do this. C's `do_exec` builds the same message and calls
`tell_vfs`, which is `asynsend3(VFS_PROC_NR, &m, AMF_NOREPLY)` plus a `VFS_CALL`
flag, then returns `SUSPEND`; `VFS_PM_EXEC_REPLY` is handled in the main loop by
`exec_restart`. The port already followed that shape for **fork** (asynsend3,
`VFS_CALL`, `EDONTREPLY`, reply in `handle_vfs_reply`) and for **exit** (asynsend3).
Exec was the outlier, and its comment ("the blocking form is equivalent for our
single-threaded servers") was the wrong assumption: single-threaded servers still
cross when one is in the send phase of a reply and the other in the send phase of
a request.

## The fix

- `do_exec` sends `VFS_PM_EXEC` with `asynsend3(..., AMF_NOREPLY)`, sets
  `VFS_CALL`, and returns `EDONTREPLY`, so PM stays in its receive loop and the
  exec caller stays suspended.
- `handle_vfs_reply` gained a `VFS_PM_EXEC_REPLY` arm: it applies the exec's
  setuid/setgid (on success) and calls `exec_restart`, which answers the caller on
  failure and does nothing on success (the kernel's `SYS_EXEC_LOAD` already made
  it runnable).

## Validation

The instrumented probe reproduced the deadlock in roughly three of four runs
before the change. After it: five of five runs reported no deadlock and the guest
reached `bash-5.3#`; `just test-interactive-bash-{x86,riscv64,aarch64}` pass;
`cargo test --workspace`, clippy and the arch suite pass.

## What is still blocking

`forward_cred_to_vfs` (`setuid`/`setgid`/`setgroups`/`setsid`) still uses a
blocking SENDREC to VFS. It has the same shape and no measured occurrence. Making
it async like the exec path needs the matching `VFS_PM_*_REPLY` arms in
`handle_vfs_reply` (C replies to the waiting process there: `OK`, or
`mp_procgrp` for `SETSID`).
