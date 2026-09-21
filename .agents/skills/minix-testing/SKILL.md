---
name: minix-testing
description: Testing patterns for MINIX/Rust OS development — three test domains, QEMU integration tests, boot tests, host test isolation. Load when writing tests, debugging test failures, or deciding where to place a new test.
---

# MINIX/Rust Testing

**Every task MUST include tests. No exceptions.**

## A change is not done until a test pins it

"Existing tests still pass" is not evidence — the suite passing on both the
old and the new code verifies nothing. A behavioral or architectural change is
done only when a test **fails on the pre-change code and passes on the new**
(a regression pin). A QEMU boot is not a regression test: nobody re-runs it
after every edit, so a change verified only by booting leaves the tree exactly
as easy to break as it was before.

**The pin must live in `cargo test` (host), not in a boot.** If the changed
logic sits behind a syscall or hardware access, refactor it to be host-
testable instead of leaving it untested:

- **Extract the pure decision logic** into a function and test that; the
  syscall-touching caller becomes a thin wrapper. Examples in-tree:
  `apply_action` (extracted from PM `do_sigaction`), `process_ksig_reply`
  (extracted from the PM NOTIFY loop), tty `sigchar`'s `#[cfg(not(test))]`
  around `send_kill`.
- **Pin cross-layer invariants at the layer that owns them.** If user-space
  logic relies on a kernel invariant (e.g. PM's "exit_proc only for
  `pending_bits == 0`" depends on "`sys_exit_handler` sets
  `p_signal_received` but never `p_pending`; `cause_sig` sets `p_pending`
  but never `p_signal_received`"), write the invariant test in the kernel,
  not just a consumer-side assertion.
- **Re-enable or delete parked/`#[ignore]`d tests** that touch the changed
  area — a disabled test is an unverified behavior.

Checklist before declaring a change done:

- [ ] A host test exercises the NEW behavior (not just "suite is green")
- [ ] That test would fail on the pre-change code (verify with `git stash`
      or by reverting the one-line fix if needed)
- [ ] Parked tests touching the change are re-enabled or deleted
- [ ] All host suites green: kernel, servers, minix-std, minix-rt, userland,
      boot-image
- [ ] `cargo clippy -- -D warnings` clean on the changed crates
- [ ] Arch-gated change (VA layout, a HAL constant, anything under `arch-*/`, a
      shared test that names an address)? Then `just test-arches` too — the host
      suite cannot see the suite that runs those

## Grep for warnings — don't eyeball the tail

`cargo build`/`cargo test` print warnings *before* the final `Finished`/
`test result` lines, so piping to `tail -N` hides them. After any validation
run on a touched crate, confirm zero warnings explicitly:

```sh
cargo build -p servers 2>&1 | grep -E "^(error|warning)"   # expect: no output
cargo clippy --workspace --all-targets -- -D warnings      # expect: Finished, no errors
```

A warning in a crate you only *compiled* (not edited) still counts: if your
change prompted the rebuild and the tree had latent warnings, fix or flag
them before handing back — the tree must be shippable at every checkpoint
(`bare-metal-debug` skill, same rule).

## Test Type / Domain Quick Reference

| Domain | Runner | Best For | Cost |
|--------|--------|----------|------|
| **Host `cargo test`** | `#[test]` on host | Pure logic: parsing, math, struct layouts, constants, state machines. No hardware or syscall access. | Instant |
| **QEMU integration** (`just test-qemu`) | `test_runner.rs` inside QEMU (ring 0) | Kernel internals: page tables, IPC, scheduler, timers, interrupts, ELF loading, grant tables, syscall dispatch. Compile with `features = ["qemu-tests"]`. | ~30s per cycle |
| **Boot test** (`just test-boot`) | In-kernel after VFS mount_root | Multi-server IPC, filesystem reads, cross-process data transfer, VFS↔MFS protocol. Feature `boot-test`. | ~30s per cycle |

## When to Use Each

```
Parser bug?                → host cargo test (instant)
IPC syscall wrong?         → QEMU integration test (add Phase G)
VFS mount_root broken?     → boot test (add assertion in boot_test.rs)
Data corruption?           → boot test with raw byte dump first
```

## Both suites, all three arches

`just test-arches` runs all six QEMU gates — `test-qemu` and `test-boot` for
x86, riscv64 and aarch64. Two separate reasons make that a routine step and not
a rare one:

- **The suites are not nested.** `test-qemu` runs the in-kernel suite
  (`crates/kernel/src/tests.rs`), `test-boot` runs `boot_test.rs`: the servers
  and `mount_root` first, then a userspace phase in which a test init execs a
  program from the image. It crosses that boundary but stops short of a shell —
  nothing types a command at it. Neither includes the other, and the host suite
  runs neither.
- **`cargo test` cannot see the first one.** `tests.rs` sits behind the
  kernel's `qemu-tests` feature and its checks are driven by its own runner, so
  the host suite neither compiles nor runs them. A test that exists only there
  is compiled by `just test-qemu-<arch>` and nothing else.

`syscall_brk` is the worked example: it asserted x86_64's heap base
(`0x3FE00000`) while aarch64's is `0x2000_0000`, so it passed on two arches and
on the host while `test-qemu-aarch64` failed. Any change to a VA-layout
constant, a HAL value, or per-arch code needs all six.

The host pin is still the pin (above) — this is the extra sweep for the
arch-gated part, not a substitute for it.

The six gates are not the same as `just test` (`test-<arch>` runs a *boot*, no
assertions) or `just check` (host clippy + one riscv64 compile). Neither of
those would have caught the example above.

## QEMU Integration Tests

File: `crates/kernel-boot/src/test_runner.rs` (40 tests, phases A–O + kernel tests as Phase H)

Pattern:
```rust
// In test_runner.rs or kernel/src/tests.rs
fn test_my_thing(ctx: &mut TestCtx) {
    // ... do something with hardware access ...

    if success {
        ctx.ok("my thing worked");
    } else {
        ctx.fail("my thing broke");
    }
}

// Then register in run_integration_tests() or run_all():
total += run("my_thing", test_my_thing);
```

Helpers:
- `ctx.ok(msg)` / `ctx.fail(msg)` — test result output
- `serial_putc()`, `serial_puts()` — raw serial output (use when TestCtx methods aren't enough)
- `qemu_exit_success()` / `qemu_exit_failure()` — exit QEMU with result code via isa-debug-exit port `0x501`

Gate with feature: `--features qemu-tests` (Cargo.toml feature, enabled by `just test-qemu`).

## Boot Tests

File: `crates/kernel-boot/src/boot_test.rs` (12 tests)

Runs inside the kernel after VFS calls `syscall1(60, 0)` (`SYS_BOOT_COMPLETE`). The kernel's handler in `main.rs` calls `run_boot_tests()` which exits QEMU via isa-debug-exit.

Pattern:
```rust
fn test_something() -> u32 {
    unsafe {
        // Read kernel state (process table, message buffers, etc.)
        let rp = kernel::table::proc_addr(SOME_EP);
        let value = (*rp).some_field;
        if value != expected {
            serial_write("  FAIL: ...\r\n");
            return 1;
        }
        serial_write("  OK description\r\n");
    }
    0
}
```

Then register in `run_boot_tests()`:
```rust
failures += test_something();
```

**Debug dump pattern** (temporary — remove after debugging):
```rust
serial_write("  DBG: bytes 8-15: ");
for i in 8..16 {
    let b = *msg.add(i);
    let hex = b"0123456789abcdef";
    serial_write(core::str::from_utf8(
        &[hex[(b >> 4) as usize], hex[(b & 0xf) as usize]]
    ).unwrap_or("??"));
    serial_write(" ");
}
serial_write("\r\n");
```

Gate with feature: `--features boot-test` (enabled by `just test-boot`).

## Host Test Isolation

| Mechanism | When |
|-----------|------|
| `#[ignore]` | Tests needing ring-0 or MINIX ABI (syscall, I/O ports) — mark ignored on host |
| `#[cfg(target_os = "none")]` | Code that can only compile for the MINIX target |
| `TestLockGuard` + `TEST_LOCK` | Serialize tests sharing global `UnsafeCell` state (IPC server tests) |

## Quick Reference

```
# Host tests
cargo test                          # all host tests
cargo test -p kernel                # single crate
cargo test my_test_name             # filtered

# QEMU tests (kernel integration), per arch - x86 by default
just test-qemu                      # 40 tests (phases A–O)
just test-qemu aarch64

# Boot tests (multi-server, filesystem), per arch - x86 by default
just test-boot                      # 12 tests after VFS mount_root
just test-boot aarch64

# All six QEMU gates (x86/riscv64/aarch64 x qemu+boot) - what an
# arch-gated change needs
just test-arches

# Normal boot
just run                            # no tests, starts shell
```

Pass/fail for the QEMU runners is decided from the guest's serial log, not from
QEMU's exit status: the riscv64/aarch64 kernels cannot report one (SBI SRST /
PSCI), so QEMU exits 0 whatever the guest did. Each `test-boot-*` / `test-qemu-*`
recipe tees the serial output to `target/test-<kind>-<arch>.log` — visible on
stdout while streaming, kept as the CI artifact — and then calls
`just _assert-qemu-log <log> <marker>`; the marker is `ALL TESTS PASSED`, or
`-- done --` for the x86 kernel suite, and the assert fails on a `FAILURES:` line
or a missing summary. Read that log when a recipe fails: stderr goes through the
same `tee`, so host-side failures (a missing `qemu-system-*`, a bad device) land
in it too and fail the same assert.

Because the recipe is a pipeline, the line's exit status is `tee`'s, not QEMU's.
The marker assert is the gate — do not add a `code=$?` check after the pipe and
expect it to see QEMU's status.

## The emulator version is a requirement

`test-qemu-*` and `test-boot-*` check their emulator before booting a guest
(`_assert-qemu-version`, floor in the `qemu-min-version` variable) and refuse
anything older than QEMU 11. That is not tidiness: on the 8.2 that ubuntu-24.04
ships, the aarch64 boot suite hangs forever at `scheduler starting...` behind a
permanent IRQ storm (966k `Taking exception 5 [IRQ]` in 20 s, all from one ELR),
so a stale emulator only looks like a slow suite until `qemu-timeout` kills it.
No distribution packages QEMU 11 yet, which is why CI builds it
(`.github/actions/install-qemu`, cached); a dev machine needs 11 or newer.
