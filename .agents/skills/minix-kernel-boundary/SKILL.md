---
name: minix-kernel-boundary
description: Traps at the kernel-to-server boundary of the MINIX/Rust port — identifying a process (endpoint vs slot vs proc_nr vs pid) and copying data into a caller's address space. Use when resolving an endpoint to a process, when scanning a table to work out who a message is about, when encoding an endpoint from a slot, when comparing a parent against a pid, or when copying kernel data to a caller's `val_ptr`; and when a message is dropped, delivered to the wrong process, or a `GETINFO` reply comes back as zeros.
---

# Kernel ↔ server boundary traps

Two families of bug with one shape: a server **inferred** something the kernel already knew, and the
inference was right until the *second* case. Both are silent — no crash, no errno, no log line — and
both were found by giving the port a second process to get wrong (findings 39, 42, 43, 44, 45 and 46
in `PORTING_PLAN.md`).

When you write code that could ask the kernel one question and get the truth, ask. This skill is
about the places where that is not obvious.

## 1. Four number spaces name one process

| Space | Type | Comes from | Notes |
|---|---|---|---|
| **slot** | `usize` | index into PM's `MPROC` (`crates/servers/src/pm.rs`) | `0..NR_PROCS` (256) |
| **proc_nr** | `i32` | index into the kernel's `Proc` table (`crates/kernel/src/table.rs`) | `-5..=255`; negatives are kernel tasks |
| **pid** | `i32` | PM's `mp_pid` | a *name* PM hands out, not a location |
| **endpoint** | `i32` | `make_endpoint(generation, slot)` | what IPC actually carries |

- **A slot is a place; a pid is a name.** `mp_parent`, `mp_tracer` and the `mproc` index are slots.
  `mp_pid` is not a slot, and `mp_parent = 1` does not mean "pid 1" — it means slot 1, which is VFS,
  not INIT (finding 46, still open in `exit_proc`).
- **The endpoint is the identity.** `endpoint_slot(ep)` is C's `_ENDPOINT_P`; for a boot process the
  generation is 0 and the endpoint equals the proc_nr, but a fork child gets
  `endpoint_gen(stale) + 1` (`do_fork_handler`), so *the same slot is a different endpoint after
  every reuse*.
- **The three agree by construction on this port**, and that is load-bearing: `init_boot_procs`
  fills `mproc[proc_nr]` from the kernel's image table, `do_fork_handler` honours PM's `child_slot`,
  and a spawned slot is declared to the kernel's image (`minix_image_add`) before PM reads it. If
  you find yourself searching for a process rather than indexing to it, suspect that invariant
  first.

### Three ways to get identity wrong

- **Scanning a table for a flag to find out who a message is about.** `handle_vfs_reply` used to
  take the first slot with `VFS_CALL` set. A flag describes *state*; when two processes are
  mid-round-trip the scan picks the first, `restart_sigs` clears the flag on that one, and the
  unrelated reply is acted on as if it were that process's. The symptom is severe and silent: a
  `fork` whose parent never gets its reply blocks forever, while the system looks idle (finding 44).
  Resolve from the endpoint the message names — VFS echoes it in `m7_i1` — and keep a scan only as a
  documented fallback.
- **Encoding an endpoint from a slot.** `slot | 0x8000` is generation 1, which is right the first
  time a slot is used and wrong every time after (finding 45). Use the endpoint the kernel assigned:
  `handle_fork` records VM's reply into `mp_endpoint`, so the table holds the truth. Note that
  `do_fork` still writes that placeholder at `pm.rs:771` — it is overwritten on every success path,
  so do not rely on it and do not copy the pattern.
- **Comparing a slot against a pid.** `exit_proc`'s reparenting reads
  `if parent == 0 || parent == 1` as "PM or INIT", and then assigns `child.mp_parent = 1` — but
  `mp_parent` is a *slot*, and slot 1 is VFS. A child that outlives its parent ends up with the
  wrong one (finding 46, unfixed).

Before adding any lookup: is the endpoint available at the call site? If yes, there is nothing to
look up.

## 2. `boot_cr3() == 0` is not "one address space"

The pattern

```rust
if crate::hal::boot_cr3() == 0 {
    core::ptr::copy_nonoverlapping(src, val_ptr as *mut u8, n);   // WRONG on wasm
}
```

reads "there are no page tables to switch" as "the kernel and the caller share one address space".
On a hardware arch before paging those are the same statement; on an arch with no page tables at all
they are not — the caller's memory is another instance, so the plain copy writes into the kernel's
own `.bss`, and the caller reads whatever was already at its address **and is told the call
succeeded** (finding 43). A `GETINFO` answer that comes back as zeros or stale bytes is this bug.

The rule: ask the HAL. `crate::hal::CROSS_ADDRESS_SPACE_COPY` is `Some` exactly where page tables
cannot join two address spaces (`crates/arch-wasm32/src/hal.rs`), and `None` on the hardware arches
where the CR3 switch does the job. Copy kernel → caller through
`kernel_to_proc` in `crates/kernel/src/system.rs`, which is the one helper for it:

```rust
unsafe fn kernel_to_proc(src: *const u8, dst_proc: i32, dst_addr: u64, bytes: usize) -> i32
```

and **return its result**: a copy that failed and replied `OK` is the same lie in a different place.
Every `val_ptr` arm in `do_getinfo_handler` goes through it now — `GET_KINFO`, `GET_IMAGE`,
`GET_PROCTAB`, `GET_PRIVTAB`, `GET_PROC`, `GET_PRIV`, `GET_MACHINE`, `GET_IRQHOOKS`,
`GET_IRQACTIDS`, `GET_MONPARAMS`, `GET_HZ`.

The pattern is still safe in four places, and each is safe for a reason worth recognising rather
than copying blindly:

| Site | Why it is safe |
|---|---|
| `crates/kernel/src/vm.rs` (`virtual_copy`) | the HAL hook was already consulted above it; this is the hardware path's precondition |
| `crates/kernel/src/system.rs` (`do_vumap_handler`) | guarded by `!via_hal` — the check only gates the CR3 path, which is where the precondition belongs |
| `crates/kernel/src/exec.rs` | a different question: is the boot page table up yet |
| `crates/kernel/src/debug.rs` | a different question: is this a test build |

## Review checklist

- Does this code decide **which process** a message is about? Then it needs the endpoint from the
  message, not a scan, not arithmetic on a slot, not a pid comparison.
- Does this code decide **which memory** an address refers to? Then it needs
  `CROSS_ADDRESS_SPACE_COPY` (or `kernel_to_proc`), not `boot_cr3()`.
- Does a flag, a name or a generation get derived rather than stored? `slot | 0x8000` and a
  `VFS_CALL` scan are the same mistake in two costumes.
- Is the failure reported? A dropped message with no diagnostic and a failed copy that returns `OK`
  both look like "nothing happened", which is how findings 43 and 44 survived so long.

## Where this is pinned

- `crates/servers/src/pm.rs`: `test_pm_isokendpt_places_registered_processes`,
  `test_pm_register_sender_registers_an_unknown_process`,
  `test_pm_register_sender_refuses_an_occupied_slot`,
  `test_init_boot_procs_registers_at_the_kernel_process_number`.
- `crates/kernel/src/table.rs`: `test_image_add_is_idempotent_and_updates_the_endpoint`,
  `test_image_table_names_every_boot_process`.
- `tools/wasm-servers/boot.cjs`: "the forking program is a process of its own, registered at the
  slot its endpoint names".
- `tools/wasm-browser/run.js`: the command loop, which asserts every child lands in the slot the
  previous one freed — the case where a stale record delivers into a dead instance.
