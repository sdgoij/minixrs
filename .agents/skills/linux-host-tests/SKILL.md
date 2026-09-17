---
name: linux-host-tests
description: Runs the host test suite and clippy on Linux in a container on the podman WSL machine, so host-libc and syscall portability bugs that a Windows-only dev loop cannot see get caught before CI. Use when reproducing a Linux CI failure locally, before pushing changes to the host test surface, or when a test, link or clippy result differs between Windows and Linux.
---

# Linux host tests (podman container)

Run the host test suite the way CI's Linux `host-tests` job does, without
leaving Windows. The whole run is one recipe:

```
just test-linux
```

It boots the podman WSL machine if needed, runs `cargo test --workspace` and
`cargo clippy --all-targets -- -D warnings` inside a `rust:$(channel)` container,
prints the test totals and finishes with a verdict line
(`linux host tests: cargo-test rc=… clippy rc=…`).

The repo is mounted **read-only** and the build lands in the container's own
filesystem, so running it never modifies the working tree. The image tag is
derived from `rust-toolchain.toml`, so it cannot drift from the pinned channel.

## When to use it

- Before pushing anything that touches the host test surface — especially
  `crates/minix-rt`, `crates/minix-libc`, `crates/minix-std`, `crates/userland`,
  the arch crates, or link/build configuration.
- When CI reports a Linux failure that does not reproduce on Windows.
- When the same test, link step or lint passes on Windows and fails on Linux.

## Why a Windows-only host run is not enough

Three bugs found this way in a single session, none visible from Windows:

1. **A host test binary shadowed a libc symbol.** `minix-rt` exported its C
   `write` under `feature = "rt"`, and workspace feature unification turns that
   on for every host test binary linking `minix-rt`. The definition overrode
   glibc's `write`, so `std`'s own stdout/stderr writes went through a Minix
   syscall number, `write_all` mis-sliced, and the panic hook recursed until the
   stack overflowed. Seven suites died with `signal: 11, SIGSEGV` and no output.
   Windows was immune only because MSVC names the symbol `_write`.
2. **A raw Minix syscall used on a host.** `userland::write_out` called
   `minix_rt::write` unconditionally. On Linux that syscall number is `close`,
   so the first test that printed closed fd 1: the binary's summary went
   nowhere, cargo reported **success**, and the suite silently stopped being a
   gate.
3. **`c_long` width.** 32-bit on Windows (LLP64), 64-bit on Linux, so
   `t as i64` on a `TimeT` in `minix-libc` is a required widening on one host
   and a redundant cast on the other — `clippy -D warnings` passes on Windows
   and fails on Linux.

The common thread: host differences live in the C library, syscall numbers and
type widths, which a Windows-only loop never touches.

## Reading the result

- **Compare the totals against a Windows run.** They should be *identical*
  (at the time of writing `passed=2618 ignored=36 summaries=36`). Counts grow as
  tests are added; equality is what matters, not the absolute numbers.
- **A lower Linux count that still exits 0 is the dangerous case** (bug 2): a
  suite ran tests but never printed its summary. Cargo cannot tell.
- `signal: 11` / `SIGSEGV` on a test binary points at a shadowed libc symbol or
  a raw minix syscall (bugs 1 and 2). Get a backtrace with gdb before guessing.
- `undefined symbol` at link time means a linker-script symbol is stubbed for
  the wrong target — `__bss_end` was once gated on `target_os = "windows"`.
- For per-suite comparison, extract each `Running …/deps/<name>-<hash>` line and
  the `test result:` line that follows it from both logs, then diff. A suite
  present on one side only means the target was gated out or never reported.

## Debugging inside the container

Replace `--rm` with `sleep infinity` and `podman exec` in. `gdb` is one
`apt-get install` away for backtraces, and `--network=host` gives working DNS
for `rustup component add clippy` and host pulls.

## Traps

- Keep `MSYS_NO_PATHCONV=1` on `podman` commands run from Git Bash, and pass
  container paths unquoted. Without it MSYS rewrites `/tmp/x` and `/mnt/c/...`
  into `C:/Program Files/Git/...` and the container sees nonsense.
- Pass the repo path Windows-style (`C:/Users/...`) and let podman translate it.
  The `:ro` mount is deliberate, which is also why the recipe passes `--locked`:
  cargo must not want to rewrite `Cargo.lock`.
- The rust image's `cargo` is not on `PATH` for a non-login `bash -c`, and
  rustup re-syncs the channel on every invocation. Set both, as the recipe does —
  `export PATH=/usr/local/cargo/bin:/usr/bin:/bin` and
  `-e RUSTUP_TOOLCHAIN=<channel>-x86_64-unknown-linux-gnu`.
- The image ships `clippy-driver` but not the `cargo-clippy` shim, so the recipe
  runs `rustup component add clippy` first (it needs `--network=host`).
- `podman machine stop` when you are done; the VM stays running otherwise.
- Fix the root cause. Do not widen `#[ignore]`s to make Linux pass: every ignore
  added for this is `not(target_os = "minix")` and justified by the test needing
  the Minix syscall ABI or real hardware. Prefer making the host path correct
  (as `write_out` now does) so the test keeps its coverage.
