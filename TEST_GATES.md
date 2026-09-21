# TEST_GATES.md — what "it boots" means, and why the boot tests should have failed

`PORTING_PLAN.md` finding 57 is the failure this is written after: all three hardware arches stopped
at `init: starting shell...`, and **every gate was green** — six QEMU suites, the host suite on two
platforms, CI, and the wasm harnesses. Nothing reported an error, because nothing was wrong except
that the system would never do anything again: the faulting process sat on `RTS_PAGEFAULT` and the
run queues were empty.

The gate that should have caught it is `test-boot`, and it did not. It could not. That is the
mitigation this document is about; the rest follows from it.

## 1. Why the boot suite could not have failed

Not weak — structurally incapable, in four separate ways.

**(a) It runs with no user process in existence.** `boot_procs()` returns `BOOT_PROCS_ALL` minus its
trailing INIT under the `boot-test` feature, deliberately, so the suite's checks see a fixed
post-`mount_root` state. So the suite runs, asserts, and exits QEMU before init has ever been
loaded — let alone exec'd anything.

**(b) The summary is printed before the boundary it needs to guard.** `run_boot_tests()` ends by
exiting QEMU with `ALL TESTS PASSED`. A log in which userspace never started contains that marker, so
the `_assert-qemu-log` gate accepts exactly the log that finding 57 produced.

**(c) Its own liveness check excludes the process that was wedged.** `test_all_boot_procs_alive`
iterates the same minus-INIT list. The process stuck on `RTS_PAGEFAULT` for the whole boot was `init`
— the one entry the check does not look at.

**(d) The path that broke is reachable *only* from a userland-initiated exec.** Boot processes are
loaded by the boot loader (`load_and_prepare_proc`), which builds each image itself and never touches
VFS. The vmfd, the lazy `VR_FILE` regions, `SYS_EXEC_LOAD` and the FDIO that demand-pages the first
fault all live in VFS's `pm_exec` path — reached when a *user* process calls `execve`. The boot suite
reads files and ELF headers through VFS, which exercises VFS↔MFS, but no amount of boot-proc checking
creates a vmfd or faults on a file-backed page.

So the suite's coverage stopped at the exact line where the bug started, and its marker meant "the
servers came up, before anyone used them".

## 2. Why that is the gate to fix

- **It runs on every PR and every arch.** `arch-tests` runs `test-qemu` + `test-boot` for x86, riscv64
  and aarch64. `image-<arch>` — the only job that boots a shipped artifact — runs only in the
  `release` job, on pushes to `main` and tags. A marker patch there would have protected the
  least-watched half of the gate on the least-frequent trigger.
- **Its declared domain is where the bug lived.** `minix-testing` describes `test-boot` as
  "multi-server IPC, filesystem reads, cross-process data transfer, VFS↔MFS protocol" — which is
  precisely the neighbourhood of a file read that never reached the filesystem.
- **It is already the in-guest suite with the machinery.** It walks page tables, reads files from the
  image, inspects the process table and prints a serial log. The assertions the fix needs are all
  built from parts it already has.

The rest of this document is therefore ordered around making `test-boot` cross the boundary it stops
at. The other causes below are real and worth fixing, but they are second.

## 3. The other five causes

**(e) The gates boot a different artifact than the one that ships.** `test-qemu-*` runs
`kernel-boot-<arch>-test` and `test-boot-*` runs `kernel-boot-<arch>-boot`; neither is the production
`minix-<arch>.elf`, which only `image-<arch>` boots. This is the argument for keeping a *second*
assertion on the shipped artifact even once the boot suite crosses the boundary (Step 2 below).

**(f) The production gate's marker precedes userspace.** `image-*` asserts `wserver: ready`, printed
by a server's startup before `init` execs anything. `#` has never appeared as a marker anywhere in
the `Justfile`; `wserver: ready` was there from the commit that introduced `image-*`
(`63c99edce`), whose message says the recipe "asserts the shell prompt with `_assert-qemu-log`". The
intent was the prompt; the code was one server's startup; nothing pinned the difference.

**(g) The doctrine overstates the coverage.** `minix-testing` says `test-boot` "boots to a shell". It
does not, for the reasons in §1. The document people trust claimed more than the code did, which is
why the marker in (f) survived review.

**(h) A marker can be satisfied by the failing system.** Two shapes: a marker from before the
boundary (f), and a marker the harness's own input produces — a gate that pipes `echo boom` into the
guest and greps for `boom` passes on a shell that echoes the line and never runs the command.

**(i) The target we iterate on is blind to the class.** wasm has no page faults, so it has no FDIO,
no demand-paged pages and no `prefault_vfs_file_regions` — the entire file-backed exec path is absent
there. Its harness is the strongest of the four (`run.js` types commands, execs files, forks, re-reads
a disk), which makes the coverage look symmetric when it is not. And no workflow runs that harness at
all: `boot.cjs`, `run.js` and `page.test.js` are manual, and `just publish-wasm` is a manual recipe.
There is a fair reason — the wasm build needs a nightly and Binaryen — but the consequence is that
the only surface asserting *usable* runs by hand.

## 4. The vocabulary that was missing

- **Booted (phase)** — every boot process reached its main loop and VFS mounted root. This is what
  `test-boot` asserts today, and it is a real gate.
- **Usable (system)** — a program the boot image provides, in its own filesystem, is exec'd by a user
  process and its output comes back. This is what a person means by "it boots".

A suite may assert either, but only the second may be the thing that says an arch works. Today no arch
gate asserts it; only the wasm harness does, and nothing runs that.

## 5. The plan

**Step 1 — make `test-boot` cross the userspace boundary.** *The mitigation. It would have caught
finding 57, on all three arches, on every PR.*
Five parts, in the order they matter:

- **Keep phase 1 exactly as it is.** The deterministic post-`mount_root` checks are the reason the
  suite is trustworthy; do not trade them for coverage. Phase 1 stays as-is and the summary stops
  being printed at the end of it.
- **Load INIT, do not release it.** The boot-test configuration loads the init entry but keeps it off
  the run queue, so phase 1 still runs with no user process scheduled. The suite releases it when
  phase 1 is done. (This is what keeps (a)'s determinism while crossing the boundary it caused.)
- **A non-interactive test init that execs a real program from the image.** Not the interactive shell
  — that needs console input and a terminal. It should `execve` something in the image and then signal
  the kernel through a second boot-test syscall, so phase 2 is event-driven rather than timed. The
  exec is the whole point: it is the only way to reach VFS's `pm_exec`, and therefore the only way to
  create a vmfd and fault on a file-backed page.
- **Phase 2 asserts the mechanism, not just the output.** Running in the kernel, it can walk the
  exec'd process's page table and require the entry VA to be *present, user and executable* — which
  is exactly what was missing when the FDIO wedged. The suite already walks page tables
  (`boot_table_walk`, "all booted procs have walkable page tables"), so the parts exist. Also: the
  exec'd process is alive, its regions describe the new image, and the run queue is non-empty.
- **A watchdog, because this is what turns a wedge into a failure.** If phase 2's signal does not
  arrive within N timer ticks, dump the process table and *fail*. Finding 57's log would then have
  ended with `FAIL: no user process completed an exec; init fl=PAGEFAULT ...` instead of silence.
  Without this part the suite still cannot fail on a hang — it just lacks a marker.

What this catches and what it cannot: the kernel cannot read VFS's bookkeeping, so phase 2 sees the
*symptom* (the entry page never became present, the marker never arrived) rather than the vnode that
was reset. That is the class, which is what a gate is for; the line is what a diagnostic is for
(Step 5).

**Step 2 — keep an assertion on the shipped artifact.** *Defence in depth; catches cause (e), which
Step 1 cannot.*
Pipe `/bin/echo <marker>` into the guest in `image-<arch>` and require a line that is exactly
`<marker>` — the echoed input is `# /bin/echo <marker>`, so only real output satisfies it (cause (h)).
Keep `wserver: ready` beside it as information, not as the gate. This is the same assertion the wasm
harness already makes, on the artifact a user downloads. Its one open question is whether input piped
into QEMU at boot is buffered by the tty until the shell reads it or dropped; if dropped, it needs a
feeder that types after the prompt appears. Step 1 does not depend on the answer, which is another
reason to do it first.

**Step 3 — make the names and the doctrine true.** *Cheap, and it stops the next reader relying on a
claim that is not checked.*
Retire `minix-testing`'s "boots to a shell" (post-Step 1 the sentence becomes true, so write what is
now true and where the boundary sits), and say in the `Justfile` what each marker means:
`ALL TESTS PASSED` = servers + one userland exec; the image marker = a command from the image
answered.

**Step 4 — test the gate.** *Protects Steps 1 and 2 from rotting the way the old marker did.*
Two fixture logs — the good one, and one stopped at `init: starting shell...` — with a check that the
markers accept the first and reject the second. A second a second, and it makes any future marker
change that weakens a gate fail loudly. A marker string is a specification and should be reviewed as
one.

**Step 5 — make the silent classes loud at runtime.** *The per-run version of Step 1's watchdog; it
would have turned today's 20-minute decode into one line.* **Done (findings 64, 65).**
- In VM's `vfs_request_sync`: the reply is consumed without checking its type, so a stray request
  delivered into the reply slot is read as a result. Validate `m_type == VM_VFS_REPLY` and report a
  mismatch (sender, type, first payload words) rather than using it. **Taken:** `is_vfs_reply` gates the
  read, a mismatch answers `EINVAL`, and the report goes out on the diag channel — deliberately not
  through VFS, which is the server it is reporting about — once, with the sender, the type, the
  request, the fd and the first two payload words.
- In the kernel: when `mini_send` would satisfy a SENDREC waiter with a message whose *sender is
  itself awaiting a reply*, two processes are in `sendrec` to each other and their messages have
  crossed. Report it once, with both endpoints and types. Target-independent, so it covers the wasm
  leg too. **Taken:** a `sendrec`'s send is marked with `SENDREC_SEND` (the caller's
  `REPLY_PEND` is not enough — finding 65 shows the flag outliving the `sendrec`), the report is once
  and the count is every occurrence, and the delivery is left alone — the invariant is VM's, and
  refusing the send would turn a wrong answer into a deadlock. Guards on both halves, including the
  negative cases: a detector that fires on ordinary traffic is worse than none.

The step's second bullet found something on its own: the first predicate fired on every boot, on a
`REPLY_PEND` that had outlived a completed `sendrec` because the async delivery path
(`try_deliver_senda`, which is how VFS answers VM) cleared the receiver's `RECEIVING` and not its
`REPLY_PEND`. That is finding 65 — C has the same hole and does not care, because C's `WILLRECEIVE`
never reads the flag; this port's does. This is the shape the step is *for*: the detector did not find
the crossing it was written for, it found the thing that would have made it lie.

**Step 6 — remove finding 58's class rather than policing it.**
The invariant today is "no allocation inside VFS's VM-request handlers", held by inspection, and a
heap growth in the wrong place wedges the system. The structural fix is the C shape: make VM's VFS
requests asynchronous (`asynsend` with `AMF_NOREPLY`, which `mini_receive`'s async path already
refuses to let satisfy a SENDREC waiter) plus the pending table `do_vfs_reply` would complete. Then
VM never blocks on VFS and the crossing cannot happen. The big one; take it with Step 5 in place.
**Done.** `crates/servers/src/vm/vfs_request.rs` is the C shape: requests go out with `asynsend3` and
`AMF_NOREPLY`, and `complete` matches the answer's request id to the request that asked for it. The
two callers that used to wait were split at the wait — `do_mmap`/`finish_mmap_file` for FDLOOKUP (the
handler returns `SUSPEND`, the completion creates the region *and* answers the caller) and
`advance_fault`/`start_file_page`/`finish_file_page` for FDIO (the fault is left open, and the page
that lands last resolves it). The exec pre-fault is the same machinery with a cursor: the pages are
asked for one at a time, and the fault's own page is the last step. Two deliberate differences from
C's `vfs.c`: a fixed node pool instead of `SLABALLOC` (allocating here is the hazard), and several
requests in flight keyed by id instead of C's one-active LIFO queue (VFS echoes the id).

What this does and does not prove: the class is gone by construction rather than by test — there is
no longer a `SENDREC` to VFS anywhere in VM's source, which is checkable by reading it — and the
paths the change touches are the ones every boot already exercises (exec runs a file-backed image,
which is FDLOOKUP plus a pre-fault of FDIOs). The new unit tests cover the table (a reply for a
forgotten request completes nothing) and the wire layout; the wedged interleaving itself is not
reproducible on demand, since provoking it needs an allocation in VFS's handler that the fix made
safe to have.

**Step 7 — one scenario, four targets.** *Bounds the wasm blind spot (cause (i)).*
Express the userspace smoke scenario once — exec a program from the image, check its output, write a
file, read it back — and run it on the three arches and in the wasm harness, reusing `run.js`'s
scenario rather than inventing a second. ~~Wiring the wasm harness into CI is a separate decision
(nightly + Binaryen on a runner)~~ **Taken.** `ci.yml` has two new jobs: `wasm-tests` installs a
nightly with `rust-src`, installs Binaryen where `build.sh` looks for it, builds the artifacts and runs
`boot.cjs`, `run.js` and `page.test.js`; `wasm-wire-tests` runs the network link's two, which need no
toolchain, no artifact and no submodule at all. Both are in `release`'s `needs`, so a red wasm run
stops a publish like any other gate. What remains of *this* step is the shared scenario: the three
harnesses and the arch suites still state their own.

**Step 8 — write down what is not covered, next to the gates.**
Today: userspace on wasm (no paging, so the file-backed exec path is absent); finding 58's invariant
until Step 6; `publish-wasm`'s *staging* not in CI — its harnesses are, since Step 7's decision, but
the staging cannot be gated by diffing `docs/` after a rebuild, because the wasm artifacts are only
byte-reproducible under the same nightly and `build.sh` pins none; and `image-release.yml` running
`just image <arch> 120` — the same assertion as the dev self-check with a 120 s timeout instead of 5,
so the released boot is longer and no stronger. Edit this list whenever a gate changes.

## 6. Order

Step 1 first: it is the mitigation, it lands in the gate CI watches everywhere, and it does not
depend on Step 2's open question. Then Step 2 (the shipped artifact), Step 3 (so the docs stop
claiming more than the code), Step 4 (so neither can rot). Steps 5 and 6 are the standing hazard and
its structural removal; Step 7 bounds the target that cannot see any of it; Step 8 keeps the
assumptions visible.

## 7. The rule

**A gate must assert the smallest thing a user would call working, and a system that stops making
progress must be a failure with a name — not silence, and not a marker from before the boundary.**

Everything above is an instance of that sentence. Finding 57 was a suite that exited before userspace
existed, printing a marker that the failing log also contained, on a binary that is not the one
users get, while the checks that might have noticed — the liveness scan — skipped the process that
was stuck.
