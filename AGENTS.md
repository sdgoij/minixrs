# Working in minixrs

minixrs is a from-scratch Rust port of MINIX 3.3.0: kernel, architecture code,
drivers, filesystem and system servers, networking, and userland, for x86_64,
RISC-V64, AArch64 and wasm32. It builds with the forked compiler in `rust/`
(`just bootstrap`, or `just fetch-stage1` for the published one), and `Justfile`
is the build runner.

This file is the working agreement: what a change must do, what gates it, and
how it is committed. Detail belongs elsewhere:

| Detail | Home |
|---|---|
| A procedure, subsystem detail, a debugging method | `.agents/skills/<name>/SKILL.md` |
| Design, measurements, negative results | `.notes/` |
| A known defect, open or resolved | `KNOWN_ISSUES.md` |
| How to build and boot | `README.md`, `just --list` |

## The iron rule: a passing test is not a passing suite

The cases that name your change can pass while one in a crate you never opened
fails. Run the whole gate for the surface you touched, in the order CI runs it,
before claiming anything is green. `.github/workflows/ci.yml` is the truth; if
this file and CI disagree, CI wins.

```sh
# 1. Host suite. Pure logic only: no minix-target code is compiled.
cargo test --workspace

# 2. Host lints, warnings as errors. --all-targets matches CI.
cargo clippy --all-targets -- -D warnings

# 3. The other host-side gates: the C header contract, the physmap rule, and a
#    riscv64 compile with the fork toolchain.
just check

# 4. Every QEMU gate on every arch: in-kernel suite, boot tests, fork, waitid,
#    C ABI, winprobe and burst.
just test-arches

# 5. The shipped artifact: build one ELF and type the userspace smoke scenario
#    into the shell that comes up.
just image <arch>
```

### What to run for what you changed

| Touched | Gate |
|---|---|
| Any Rust | steps 1 to 3 above, plus 4 when the change is arch-gated |
| Kernel, `arch-*`, a HAL constant, a VA layout, IPC, a shared test that names an address | `just test-arches` |
| Image assembly (`boot-image`, `crates/kernel/build.rs`, `tools/mkboot.rs`, boot config, initramfs or minixfs contents) | `just image <arch>` and `just test-arches` |
| A server or userland path compiled only for minix | `just build-<arch>` and the matching smoke gate; host clippy cannot see the code |
| Dynamic linking or `ldso` | `just test-dynlink-<arch>`, `cargo test -p ldso` |
| Mesa, libdrm or the GL/EGL stack | `just test-gltriangle <arch>` (see `mesa-port`) |
| bash or readline | `just test-interactive-bash <arch>` |
| wasm front ends or the network link | `sh tools/wasm-verify.sh`, `node tools/wasm-browser/net.test.js`, `node tools/wasm-net/relay.test.js` |
| The rust fork or the toolchain pin | `just bootstrap`, `just verify-stage1` (Linux/WSL), then the arch suites; see `rust-fork-rebase` |
| Documentation only | no build gate; verify links and claims by hand |

### Traps that keep catching us

- **`cargo test` cannot see the in-kernel suite.** `crates/kernel/src/tests.rs`
  sits behind `qemu-tests`, and `test-boot` runs `boot_test.rs` instead. A test
  that exists only there is compiled by `just test-qemu-<arch>` and nothing
  else. `syscall_brk` is the worked example: it asserted x86_64's heap base and
  failed on aarch64 alone while the host and two arches were green.
- **Host clippy never compiles `#[cfg(target_os = "minix")]` code.** It is not a
  substitute for `just build-<arch>` on a minix-only path.
- **The serial log is the verdict on riscv64 and aarch64.** Neither reports an
  exit status through SBI reset or PSCI on this QEMU build, so a killed run
  still exits 0. `_assert-qemu-log` is what turns the guest's own marker into a
  pass; a missing marker or a `FAILURES:` line is a failure, never something to
  reclassify with a longer timeout.
- **Boot-process checks stop where userland starts.** The VFS/exec path is
  reachable only from a userland `execve`, so a suite that reads files through
  VFS can still miss a broken exec. `.notes/test_gates.md` is the worked
  example.
- **A physical address is not a pointer.** The kernel runs on the calling
  process's page tables, so a user mapping can shadow a frame. Convert through
  `phys_to_virt`/`frame_ptr`/`table_ptr`, or carry `physmap-ok: <reason>` where
  a cast is genuinely safe. `tools/check-physmap.py` is the gate; `minix-physmap`
  is the detail.
- **The C headers in `tools/c-include` are a contract** for bash, libc++, and
  the C/C++ smoke tests. `tools/check-c-headers.py` catches an export and its
  declaration drifting apart.
- **Windows host tests and Linux host tests differ.** `just test-linux` runs the
  Linux suite and clippy in a read-only container; use it before pushing a
  host-side change (see `linux-host-tests`).
- **A stale cargo cache hides a rebuilt compiler.** After `just bootstrap`, an
  incremental build can keep rlibs built by the previous toolchain and fail
  later with `E0463` or a clap `TypeId` panic. `_finish-bootstrap` cleans both
  workspaces for that reason; do not skip it.
- **MSYS converts POSIX-style values handed to native tools.** `MINIXFS_EXTRA`
  and `DYNLINK_BINS` must stay in `MSYS2_ENV_CONV_EXCL`, or a destination turns
  into `/bash` and every listing of `/bin` says the file is missing.
- **Tests share global mutable state.** `RUST_TEST_THREADS = 1` in
  `.cargo/config.toml` is deliberate. Do not make tests parallel, and do not
  depend on cross-test ordering.
- **Interactive recipes block forever.** `just run`, `just desktop`,
  `just debug` and a bare `qemu-system-*` never return. Run them bounded, or
  prefer the recipe's gate, which times out and prints a verdict.

## Change workflow

1. Make one focused change.
2. Add or update a test that fails without it. Prove the test is not vacuous:
   revert the change (or mutate the branch), watch the new test fail, then
   restore. A change without such a test is not done.
3. Refresh editor diagnostics project-wide, then run every gate in the table
   above for the surface you touched.
4. Commit only a green, tested tree. When the task includes committing, do not
   ask again for permission.
5. Stop and ask only when a decision, a spec ambiguity, or a missing environment
   genuinely blocks progress. Ask before changing a public interface (a syscall
   number, an ABI, a boot configuration, a header in `tools/c-include`) or a
   gate's marker.

Do not claim a gate passed unless the command was run in this session and its
result was observed. If a gate fails, fix the root cause when it is clear;
otherwise report the failing command and the relevant output.

## Test layers

Every layer has a home. None substitutes for another, and an end-to-end case is
never the only test for a pure rule.

- **Host `cargo test`**: parsing, struct layouts, constants, state machines,
  allocator logic, the loader's rules on the host. Instant.
- **QEMU integration** (`just test-qemu-<arch>`): the in-kernel suite behind
  `qemu-tests`. Page tables, IPC, scheduler, timers, syscalls, ELF loading,
  grant tables.
- **Boot test** (`just test-boot-<arch>`): servers and `mount_root`, then a
  userland exec from the image. The boundary the host suite cannot cross.
- **Shipped image** (`just image <arch>`): the artifact a user downloads, with
  the shared smoke scenario typed into its shell.
- **wasm harnesses**: boot at quiescence, a live prompt, the page under a stub
  DOM, and the link's own frames and relay.

The host pin is the pin: if logic sits behind a syscall or hardware access,
extract the pure decision into a function and test that, with the
syscall-touching caller as a thin wrapper. A QEMU boot is evidence, not a
regression test; nobody re-runs it after every edit. See `minix-testing`.

## Fixtures, markers and oracles

- `tools/smoke/scenario.tsv` is one userspace scenario read by four targets:
  the image boot on each hardware arch and `run.js` on wasm. A step added there
  is covered everywhere. A per-target scenario (`dyn.tsv`, `fbmmap.tsv`,
  `cdyn.tsv`) exists only because that target lacks the facility, not because it
  was easier to write a second file.
- `tools/smoke/feed.sh` drives a guest, and its expectations are part of the
  scenario. Input sent before the prompt is dropped, `read` eats a trailing tab,
  and a pipeline's left side can swallow an `exit 1`. `silent-failure-traps` is
  the detail.
- `.refs/minix-3.3.0/` is the C oracle. Expected behaviour comes from MINIX
  3.3.0 or POSIX, never re-derived from our implementation. A deliberate
  deviation is recorded in `.notes/`, not silently enshrined.
- `KNOWN_ISSUES.md` item numbers are stable across the open/resolved move.
  Fixing an item moves it under `# Resolved issues`; it is not renumbered and
  not deleted.
- A gate marker (`ALL TESTS PASSED`, `-- done --`) and a `tools/smoke/*.tsv`
  expectation are contracts. Changing one is deliberate and moves every reader
  in the same commit.

## Long tests, timeouts and external tools

- A timeout is a verdict, not a reason to raise the deadline. The guest suite
  gets `qemu-timeout` after the build; anything that cannot finish inside it is
  a real result.
- The emulator floor is `qemu-min-version`. An older QEMU fails by reporting its
  version, rather than hanging the aarch64 suite and looking like a timeout.
- Every gate prints a verdict. A gate that never spoke has failed, even if its
  exit status was 0; a crash after its verdict does not undo the verdict.
- A missing tool (clang, QEMU, Binaryen, a linker, a driver) is reported by
  name. Never turn it into a silent pass, and never make a gate depend on a
  machine-local path.
- Long cases still run in CI. Do not park one behind a second command that
  someone has to remember.

## Diagnostics and self-testing

- The system is its own test rig: `just test-qemu`, `just test-boot`,
  `just image`, the wasm harnesses and `just check`. Prefer adding an assertion
  to an existing gate over a new manual boot, and prefer a gate that prints a
  verdict over a transcript a human has to read.
- Diagnostic switches are off unless set and write to stderr. A debug dump must
  not become the only way a test passes.
- If a behaviour matters and can only be seen in a boot, add the assertion to
  the suite that owns it. If it still cannot be pinned, record it in
  `KNOWN_ISSUES.md`; do not leave it as folklore.
- Long comments in the code explain **why** a number is what it is, what was
  tried first, and what it cost. When you move a constant or a rule, move the
  paragraph that explains it.

## Code standards

- Correctness and clarity first. Speed and efficiency are secondary unless
  specified.
- Do not write organizational comments that summarize the code. Comments
  explain why for a non-obvious reason.
  - No decorative separators, banners, or ornamental formatting, in any comment
    style (`//`, `///`, `//!`, `/* */`). A comment that draws a line, a box, or
    a repeated symbol for visual effect is prohibited.
  - Do not remove an existing comment without a stated reason (the code it
    describes is gone, or it is factually wrong).
- Prefer implementing in an existing file unless the change is a new logical
  component.
- No panicking functions on runtime paths: return `Result`/`Option` and use
  `?`. `unwrap`/`expect` only for a genuinely infallible case.
- Watch for panics from indexing out of bounds.
- Never silently discard an error with `let _ =` on a fallible operation.
- Avoid creative additions unless explicitly requested.
- Use variable shadowing to scope clones in async contexts.
- Prefer `x.clamp(min, max)` over `x.max(min).min(max)`.
- Always run `cargo clippy --all-targets -- -D warnings` after every change,
  and resolve every warning. A warning in a crate you only compiled still
  counts.
- Never use an em dash. Use a comma, a colon, parentheses, or a new sentence, in
  comments, notes, and commit messages alike.
- No stubs: `todo!()`, a fake implementation, or a deferred path does not reach
  `main`. See `minix-c-to-rust` for the deferral rules.
- A feature that needs a newer Rust bumps `rust-version`, `rust-toolchain.toml`
  and the `rust/` pin in the same commit.

## Project invariants

These are traps a change must avoid, not a map of the code.

- **VM must stay in `BOOT_PROCS_ALL` and before every process that calls
  `brk()` (MFS, PFS, TTY, INIT).** It manages those allocations over IPC;
  removing it or reordering it breaks allocation for the whole boot. The list
  is shared by all three hardware arches, so a boot config that names process
  order keeps the same constraint.
- **The physmap is not the identity map with a prefix.** Identity stays a small,
  named exception for the trampoline, early boot, and the boot test's archive
  reads. A new kernel mapping goes through the physmap helpers, not a raw cast.
- **The kernel runs on the calling process's page tables.** A copy into a
  caller's space uses that process's page tables, not the current process's.
  See `minix-kernel-boundary`.
- **The C header contract moves with the export.** A changed export updates
  `tools/c-include` and every C/C++ consumer in the same commit.
- **`target/` is generated.** No assembled image, rlib, wasm build, stage1
  download, or compiler artifact is committed.
- **Tests are single-threaded on purpose.** `RUST_TEST_THREADS = 1` protects
  shared global state; do not relax it to make a test faster.

## Commit rules

Follow `.agents/skills/git-commit-messages/SKILL.md`. The short version:

- Imperative subject with a type prefix (`feat:`, `fix:`, `test:`, `refactor:`,
  `docs:`, `chore:`), no trailing period, and aim for 50 characters.
- A body only when it carries real information: what changed and why, with
  concrete file names or behaviour. Each body paragraph is one unwrapped line.
  Never hard-wrap at a column.
- No em dash. No bare plan identifier in the subject; a phase, stage, slice or
  task number belongs in the body next to the `.notes/` document that defines
  it.
- Do not commit a CRLF-only diff. `.gitattributes` pins shell scripts to LF;
  new text files are LF too.
- Never commit generated artifacts. Keep the tree clean after committing.
- Ask before committing when the change alters a public interface or when a
  required gate cannot be run.

## Rules hygiene

`AGENTS.md` is read by every session, so it stays a short, high-signal list of
traps a change must avoid or follow. It is not a place for documentation, an
architecture map, a procedure, or a measured negative result.

- A new rule must be **non-obvious**, **repeatedly encountered**, and
  **specific enough to act on**. A lesson about how to work is a procedure:
  write a skill, not a rule.
- If a rule needs more than a sentence or two to explain, it is a skill wearing
  the wrong name. Write the skill and leave a one-line pointer here.
- A rule that applies to one crate belongs in that crate's own `AGENTS.md`.
- Do not add rules inline during feature or fix work. Propose them in the pull
  request under "Suggested AGENTS.md additions"; reviewers decide.
- A rule emerges from a validated pattern, not a one-off observation. The path
  is: note it, validate it in review, then add it in a dedicated commit that
  explains why it exists.

## Review checklist

- Is there a test that fails without this change, and does it live in
  `cargo test`?
- Was the whole gate run for the surface in the table, not only the test named
  after the change?
- For an arch-gated change, did `just test-arches` pass?
- For an image or packaging change, did `just image <arch>` boot the shipped
  artifact?
- For a new gate or harness: does it print a verdict, time out, and fail on a
  missing tool?
- Are the physmap and C-header gates clean?
- Is there no `todo!()`, stub, or `unwrap` on a runtime path?
- Does the commit message follow the skill (imperative, no em dash, unwrapped
  body)?
