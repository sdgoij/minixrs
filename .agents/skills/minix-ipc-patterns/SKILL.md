---
name: minix-ipc-patterns
description: MINIX IPC message formats, SENDREC semantics, notification system, and grant tables. Use when debugging IPC deadlocks, message corruption, or cross-process data transfer.
---

# MINIX IPC Patterns

## Message Struct Layout (ABI-Critical)

The `Message` struct (`crates/arch-common/src/ipc.rs`) is exactly **56 bytes** with `#[repr(C)]` layout:

```
Offset  Size  Field
0       4     m_source    (i32) — sender endpoint
4       4     m_type      (i32) — message type / request number
8       48    m_payload   (union Payload) — message data
               ├─ m1: M1 (12 × i32)        — generic message fields
               ├─ m2: M2 (2×i32 + i64×3)   — longer int pairs
               ├─ m3: M3 (7×i32 + 16×i16 + 8*u8) — strings + shorts
               ├─ m4: M4 (6×i64)           — all 64-bit
               ├─ m5: M5 (6×i64)           — system call args
               ├─ m6: M6 (6×i64)           — all-purpose
               ├─ m9: M9 (4×i64 + 4×i32 + 2×i64)
               └─ raw: [u8; 48]            — raw byte view
```

**Key rule:** `m_source` and `m_type` are at fixed offsets 0 and 4. All payload variants start at offset 8.

## Message Type Ranges

Message types distinguish which subsystem handles the request:

| Range | Subsystem | Examples |
|-------|-----------|---------|
| `0x0000–0x00FF` | PM messages | `PM_FORK=0x0002`, `PM_EXEC_NEW=0x0003`, `PM_WAITPID=0x0004` |
| `0x0100–0x01FF` | Kernel calls | `SYS_FORK=0x0101`, `SYS_EXEC=0x0102` |
| `0x0200–0x02FF` | FS messages | `FS_BASE=0x0200`, `REQ_READSUPER=0x0201` |

## IPC Call Types

| Syscall | Number | Behavior |
|---------|--------|----------|
| `SEND` | 46 | Blocking send — blocks until target calls RECEIVE |
| `RECEIVE` | 47 | Blocking receive — blocks until a sender arrives |
| `SENDREC` | 48 | Atomic send+receive — sends, then blocks waiting for a reply |
| `SENDNB` | 49 | Non-blocking send — returns `ENOTREADY` if target not receiving |
| `NOTIFY` | 45 | Asynchronous notification — queued in bitmask, no data payload |

### SENDREC Semantics (C Minix behavior)

SENDREC is the most common IPC pattern (client → server → reply). Correct implementation:

1. `mini_send(target, msg)` — copy message to target's `p_sendmsg`, set target's `SENDING` flag
   - If target is `RECEIVING`: deliver immediately, clear target's `RECEIVING`, wake target
   - If target is not `RECEIVING`: queue caller on target's `caller_q`, caller gets `SENDING` set
2. `mini_receive(ANY)` — called with `SENDING` flag still set
   - **Skip** `caller_q` processing (messages from other callers)
   - **Skip** notification processing (notifications from other processes)
   - Block until reply arrives from the SENDREC target
3. When reply arrives: clear `REPLY_PEND`, copy message to caller's `p_delivermsg`

**Critical subtlety:** The SENDREC caller should NOT process caller_q entries or notifications while waiting for the reply. Only the reply from the SENDREC target counts.

Since IPC fixes in this session:
- `MESSAGE_SIZE` = 56 bytes (was 64, causing stack corruption)
- RECEIVE clears REPLY_PEND in `do_sync_ipc`
- `receive_done` label clears REPLY_PEND
- `mini_receive` skips caller_q/notifications when SENDING set
- `mini_receive` skips notifications when REPLY_PEND set

## Notifications vs Messages

| Aspect | Message | Notification |
|--------|---------|-------------|
| Data payload | 48 bytes via `m_payload` | None (bitmask only) |
| Delivery | Blocking (SEND/SENDREC) | Asynchronous (always delivered) |
| Queueing | Per-sender caller_q | OR'd into destination's notification bitmask |
| Wake-up | `RECEIVING` target woken | `RECEIVING(ANY)` target woken |
| Used for | IPC requests/responses | Hardware interrupts, urgent signals |

## Grant Tables & SAFECOPY

Grant tables allow processes to share memory buffers safely:

- **Registration:** `minix_rt::kernel_call(34, &mut msg)` — `NR_KERNEL_CALL` (syscall 50) dispatches to `do_setgrant_handler`
- **Do NOT** use `crate::ipc::sendrec(SYSTEM, msg)` — this bypasses the `NR_KERNEL_CALL` dispatch and sends an out-of-range syscall number
- **Grant ID:** assigned by the kernel when the granter process registers its grant table

### Safe Copy Flow

```
MFS → kernel_call(SAFECOPYTO_CALL, ...)
  → do_safecopy_to_handler()
  → grants::safecopy()
    → vm_check_range() — page table walk of source pages
    → verify_grant() — read grant entry from granter's table
    → virtual_copy() — bounce-copy through kernel stack with CR3 switching
```

**Known issue:** `virtual_copy()` currently produces zeros in the destination buffer. Root cause is unclear — likely related to the identity-mapped kernel layout where per-process page tables may map different physical frames at the kernel stack VA. See BLOCKERS.md for details.

## Endpoint Numbers

```
PM     = 3    (0x0003)
RS     = 4    (0x0004)
VFS    = 5    (0x0005)
VM     = 6    (0x0006)
MFS    = 7    (0x0007)
DS     = 9    (0x0009)
TTY    = 10   (0x000A)
SCHED  = 11   (0x000B)
INIT   = 12   (0x000C)
```

Defined in `crates/arch-common/src/com.rs` and re-exported through `arch_common::com::*`.
