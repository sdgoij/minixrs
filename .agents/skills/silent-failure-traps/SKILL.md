---
name: silent-failure-traps
description: Traps in minixrs where a failure looks like success or like silence rather than an error — host scripts driving a QEMU guest (an MSYS FIFO that a native QEMU reads nothing from, a pipeline's left side swallowing `exit 1`, `read` eating a trailing tab, input sent before the prompt being dropped, an expectation another step has already printed so a command that never ran passes), and the kernel's message/diag boundaries (a delivered message's source endpoint, the SYS_DIAGCTL layout). Load when writing or fixing a gate, a harness, an `image-*` recipe or CI check, when scripting input into a guest, when a guest's output stops for no apparent reason, or when a reply reaches a server with the wrong sender.
---

# Silent failure traps

This port fails quietly more often than it crashes. A gate goes green, a guest prints its prompt and
then ignores every byte, a request is answered by the wrong handler, a diagnostic line loses its first
four characters — and nothing reports an error at any point. Each trap below was measured rather than
reasoned about; the fix recorded is what the code does now.

## Driving a guest from a host script

- **Input written before the shell is reading the console is dropped, not buffered.** Measured on the
  shipped x86 image: `printf '/bin/echo alive\n' | qemu …` prints nothing, while the same bytes after a
  delay print the command, its output and the next prompt. Wait for the prompt, then send one step at a
  time and wait for that step's answer (`tools/smoke/feed.sh`).
- **A path that MSYS emulates is not readable by a native binary.** `mkfifo` under git-bash produces a
  FIFO that MSYS programs (`cat`) read fine and that the QEMU there — a native `PE32+ executable`,
  check with `file "$(command -v qemu-system-x86_64)"` — reads nothing from. The symptom is not an
  error: the guest boots, prints `wserver: ready` and its prompt, and then ignores every byte. Hold
  the write end of a shell pipe (`|`) instead. Same family, same platform: spell `/usr/bin/timeout`
  out, because a bare `timeout` is the Windows one.
- **The left side of a pipeline is a subshell.** Its variables do not come back, and its `exit 1`
  fails nothing at all — a step that stopped the guest reports as a green recipe. Write a one-line
  status file and read it in the parent, and make the parent's backstop *outlast* the driver's budget,
  or the precise verdict ("step 2 did not answer") loses the race to the vague one ("never reported").
- **`read` strips trailing IFS whitespace**, so a final empty field silently disappears — which is why
  `IFS= read -r line` comes first and the tab is split out afterwards. A leading-space field, or a
  space where a tab belongs, folds an expectation into its own command.
- **Anchored expectations only.** The guest echoes what you type, so an unanchored search for the
  expected output also matches the echo of the command that was supposed to produce it. Match a whole
  line (`^…[[:space:]]*$`) and never let an expectation be a substring of its own command.
- **An expectation can be answered by another step.** The match runs against the whole accumulated log,
  so a step whose expected line any *earlier* step has already printed passes while doing nothing at
  all. A `pr` bisect went green in all five steps with `pr` out of `feat_minix`, and the replay of the
  original failing sequence went green too — its `pr` step's expectation had already been printed by
  the `cat` step above it. An empty expectation is weaker still — it asserts only that the shell echoed
  the line and came back to a prompt, and the exit status is never consulted, so a command that
  panicked, failed or does not exist passes exactly like one that worked. Give every assertion a line no
  other step can produce (a per-step marker turned that same probe into a wedge it could finally see),
  and read "the prompt came back" as liveness, not as evidence that the tool worked.

## The kernel's message boundary

- **A delivered message's source endpoint is the message, not a return register.** `mini_send`,
  `try_one` and `try_deliver_senda` stamp the sender's endpoint into `p_delivermsg` *and* into the
  receiver's return register. A receive path that delivers a message and then returns `OK` has that
  register overwritten by the syscall epilogue, and a receiver that takes the sender from its return
  value — VFS's `get_work` does — reads 0, which is PM, and routes the request to its PM handler. The
  request is then never answered, with no error anywhere (finding 66). Return the endpoint read back
  out of the delivered message.
- **`SYS_DIAGCTL`'s layout is a contract between two crates.** Length at `msg[12..16]`, bytes from
  `msg[16..]`, `DIAG_CHUNK_MAX = 64 - 16`. The writer is `minix-rt`'s `diag_message`, the reader is the
  kernel's `do_diagctl_handler`, and moving one without the other cuts bytes out of every line without
  failing anything — a diagnostic line being what a wedged boot has instead of a stack trace
  (finding 67).

## A gate nobody has seen fail is a gate nobody knows works

Before believing a new gate, break the thing it claims to check and watch it fail *by name*. The
step-naming path in `tools/smoke/feed.sh` was verified exactly that way: with an expectation the guest
could never print, the recipe exits 1 and reports the step, its command and what it expected.

Then check what the *guest* was doing at the moment of failure — the process list, the state it is
blocked in, the log's tail. A guest sitting at a prompt that ignores input is a harness problem (the
list above); a server that never printed its ready line is a system problem, and `wserver: ready` is
asserted beside the scenario for precisely that reason (finding 66 was a dead window server under a
working shell).

## Where the reference implementations are

| What | Where |
|------|-------|
| Host-side driver that gets all of the above right | `tools/smoke/feed.sh`, `tools/smoke/scenario.tsv` |
| The same scenario inside the wasm engine | `tools/wasm-browser/run.js` |
| A delivery's endpoint | `crates/kernel/src/ipc.rs` (`mini_receive`'s async branch, `try_one`) |
| The diag channel's two halves | `crates/minix-rt/src/lib.rs` (`diag_message`), `crates/kernel/src/system.rs` (`do_diagctl_handler`) |

Related: the `minix-testing` skill covers where tests live and which suite to reach for; this one is
about the mechanics that make a test or a harness report the wrong thing.
