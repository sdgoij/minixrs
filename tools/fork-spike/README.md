# Fork spike — can a suspended wasm process be cloned?

Answers the one question that could invalidate the `arch-wasm32` design
(`ARCH_WASM32.md`, §11 risk 1 and §12): **can `fork` be implemented for a
process suspended inside a syscall, given that wasm cannot copy a running
stack?**

Run it:

```sh
sh tools/fork-spike/run.sh
```

## Verdict

**Yes.** A process suspended by Binaryen's Asyncify can be forked by cloning its
linear memory into a fresh instance; both instances rewind and diverge exactly as
parent and child should. 20/20 checks pass, including a negative control that
confirms the serialised stack is load-bearing rather than incidental.

The mechanism works because `asyncify_start_rewind(ptr)` re-establishes both
Asyncify globals *from the memory-resident struct*, so a brand-new instance needs
no global copying — the entire continuation is recoverable from the memory image.

## How the port would use it

The whole state machine is host-drivable, which is a better fit for a microkernel
than the emscripten flow. The kernel is the entity that knows a syscall must
block, and it already implements the syscall gate, so it can drive all four
transitions and the guest stays ordinary straight-line code:

```js
host_sendrec: (msgPtr) => {
  if (!blocked) {
    blocked = true;
    inst.exports.asyncify_start_unwind(dataPtr);   // state -> UNWINDING
    return 0;                                     // caller unwinds first
  }
  const reply = readI32(memory, msgPtr + 12);     // kernel's reply
  inst.exports.asyncify_stop_rewind();            // state -> NORMAL (see below)
  return reply;
}
```

`recv()` in a user process never learns it was suspended.

## Protocol, read out of the instrumented module

Verified against Binaryen 132 by disassembling the output (`wasm-dis`), not
assumed from documentation.

`wasm-opt --asyncify` adds exactly **two mutable i32 globals** and exports five
hooks:

| Hook | Effect |
|---|---|
| `asyncify_start_unwind(ptr)` | state = 1 (UNWINDING), data = ptr |
| `asyncify_start_rewind(ptr)` | state = 2 (REWINDING), data = ptr |
| `asyncify_stop_rewind()` | state = 0 (NORMAL) |
| `asyncify_stop_unwind()` | state = 0 (NORMAL) |
| `asyncify_get_state()` | reads the state global |

**The only writes to the state global in the whole module are inside those
hooks.** No instrumented function writes it. That is what makes the state machine
host-drivable: the guest needs no cooperation at all.

`asyncify_data` layout, at the pointer passed to the hooks:

| Offset | Field |
|---|---|
| +0 | `stack_ptr` — current position, **ascends** as frames are pushed |
| +4 | buffer end — the hooks assert `stack_ptr <= end` at the *start* of the call |
| +8 | buffer start (not touched by the hooks' assertion) |

### The one detail that is not obvious

The instrumented caller dispatches on the state *after* a call returns:

```
if (state == 2) { ...replay the call site... }
if (state == 0) { return }    // normal continuation
unreachable                    // anything else traps
```

So when the awaited operation finally completes, the state must be returned to
NORMAL **before the import returns**, or the caller re-enters the rewind path and
traps with `unreachable`. Emscripten does this from the guest's runtime wrapper;
with a raw module the host's import is the only place that knows the wait is over.
Omitting it was the one real bug found while building this spike.

### The other one: overflow is silent

`asyncify_start_unwind` asserts the *initial* `stack_ptr` is within bounds, but
**the unwind path then pushes frames without re-checking**. Measured: a 64-byte
buffer with a 512-frame stack suspended normally and overwrote **8184 bytes past
the buffer end** — no trap. An undersized buffer corrupts the process's own
memory rather than failing.

Two mitigations, both worth doing in the port:

- **Size it from the process's stack limit.** Serialsation is reliably
  ~36 bytes/frame (below), so a 4 MiB stack limit needs roughly a 2–2.5 MiB
  buffer. Budgeting "about as much as the maximum stack" is the safe rule.
- **Place the buffer flush against the end of committed linear memory**, so an
  overflow runs past the memory's end and traps instead of landing in the
  process's own data. This converts a silent corruption into a loud one, which
  is the difference that matters for debugging.

## Sizing, measured

Serialisation is **linear in stack depth**, fitted over depths 1/16/128/1024:

```
bytes = 60 + 36.1 × depth
```

| Depth | Bytes serialised |
|---|---|
| 1 | 96 |
| 16 | 636 |
| 128 | 4668 |
| 1024 | 36924 |

So **~36 bytes per frame plus 60 bytes fixed**. Implied capacity: 64 KiB covers
~1,800 frames, 1 MiB ~29,000, 4 MiB ~116,000. For scale, MINIX's
`DEFAULT_STACK_LIMIT` is 4 MiB (`arch-common/src/sys_config.rs`).

Fork cost (snapshot + instantiate + copy, one-frame process) ran 1.3–2.5 ms,
essentially flat in depth because the copy dominates — but that is a
*byte-copy of a 4 MiB memory*, so it is dominated by the memory size rather than
the stack. **Reducing the per-process memory size is the lever on fork cost, not
reducing the stack.** Measuring that properly needs a real workload.

## Host-side fork mechanics

Four things the host must get right, three of them silent when wrong. All are
asserted in the harnesses.

1. **Copy the snapshot in *after* `instantiate()`.** Instantiation re-applies data
   segments. A child that never receives the snapshot restarts from the entry
   point — so it re-issues the fork request and forks recursively.
   (`clonefork.js` demonstrates the consequence instead of just asserting it.)
2. **The child's memory must be the same size as the parent's**, or a parent that
   grew past the module minimum is truncated at the fork point.
3. **Duplicate the host-side per-process bookkeeping, not just the memory.**
   Memory is not the whole process. In the harness the kernel's record of "this
   process is blocked on a syscall the kernel already owes a reply to" must be
   copied, or the child re-blocks on the syscall it had already been waiting for.
   In the real port that is the `Proc` record — which fork duplicates anyway.
4. **Write divergent replies into each instance's message buffer before
   resuming.** This is the entire mechanism by which parent and child diverge;
   for real IPC it is identical to how the kernel already delivers messages.

### The mutable-global worry was unfounded

`ARCH_WASM32.md` flagged that mutable wasm globals are not part of linear memory
and so are not cloned. For this case it does not bite:

- Asyncify's own two globals are re-established by `asyncify_start_rewind(ptr)`.
- `__stack_pointer` is **already correct in the child**, because a fully unwound
  stack returns to its base before the export hands control back. Measured:
  parent `1048576`, child initial `1048576`.
- Any remaining user globals can be synchronised by the host if they are exported
  (`clonefork.js` demonstrates setting an exported mutable global across a fork).

Still worth a port invariant: **keep fork-relevant mutable state in linear
memory, not in wasm globals.**

## Layout

```
guest/            wasm32 guest, stock toolchain, no_std
  src/lib.rs        process_main            — hand-rolled continuation (layer 1)
                    async_process_main      — straight-line, for Asyncify (layer 2)
                    async_deep_process_main — suspends at the bottom of a deep stack
host/clonefork.js   layer 1: snapshot/clone/resume + the ordering gotchas
host/asyncify.js    layer 2: transform, baseline, fork, negative control, sizing
run.sh              build + run both layers
```

Layer 1 deliberately needs no Binaryen, so the host mechanics stay testable
without it. Layer 2 skips with an install hint if `wasm-opt` is missing.

A note on the deep-stack probe: `#[inline(never)]` alone is not enough to make
recursion real. Without `core::hint::black_box(&scratch)` keeping a stack slot
live across the recursive call, LLVM flattens the recursion into a loop and the
probe reports a constant size at every depth — which is exactly what it did on
the first attempt here.

## What this does *not* prove

- **Repeated suspend/resume cycles** on one process, which is what a server's
  main loop does. Every case here suspends exactly once.
- **Real fork cost** on a realistic workload. The measurement above is dominated
  by copying a 4 MiB memory and says more about memory size than about Asyncify.
- **The Asyncify runtime expansion** on a real server. 968 → 1562 bytes on a
  trivial module is not a meaningful data point; the design assumes 2–3x and that
  is still unmeasured.
- **Interaction with signals**, which the design proposes delivering at
  syscall-return time (§6.3). A rewind boundary is a natural place, but this
  spike does not exercise it.
- **Forking a process with open state to duplicate** — file descriptors, granted
  capabilities, in-flight messages. Those live in the `Proc` record and the VFS,
  and are ordinary MINIX fork concerns rather than wasm ones, but they are what
  turns "the mechanism works" into "`fork()` works".

Suggested next step: re-run with a multi-suspend process (server main loop shape)
and with a reduced per-process memory size, to get a cost figure that means
something.
