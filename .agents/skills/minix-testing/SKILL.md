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

# QEMU tests (kernel integration)
just test-qemu                      # 40 tests (phases A–O)

# Boot tests (multi-server, filesystem)
just test-boot                      # 12 tests after VFS mount_root

# Normal boot
just run                            # no tests, starts shell
```
