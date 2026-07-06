---
name: bare-metal-debug
description: Debugging methodology for bare-metal MINIX/Rust OS development. Use when investigating hangs, crashes, silent failures, or corrupted data in the kernel or server processes running on QEMU.
---

# Bare-Metal Debug Methodology

## Core Principle: Cheapest Test First

Every QEMU cycle costs ~30–120 seconds. **Do not theorize for 10 minutes what you can disprove in one boot test.** Before writing a paragraph of analysis, write 5 lines of boot test code.

```
Bad:  "virtual_copy must be broken because CR3 switches..."
Good: "Let me add a test that reads MFS's delivermsg and dumps bytes 8-31"
```

## Debugging Ladder — Start at the Bottom

When data is wrong, start from the **source** and work outward. Do not skip rungs.

```
1. Is the source data correct?      (FS image, initramfs, hardcoded constant)
2. Is the reading mechanism intact?  (function called? returns OK?)
3. Is the transport working?         (IPC send/receive, shared memory, grants)
4. Is the receiver parsing right?    (struct offsets, endianness, message layout)
5. Is the consumer using it right?   (caller checks return value? casts correctly?)
```

Concrete example from this project — root directory size was zero:

| Rung | Question | How to Test | Cost |
|------|----------|-------------|------|
| 1 | Does the FS image have a valid root inode? | `od -A x target/minixfs.img` at inode block offset | 0s (host) |
| 2 | Does MFS call `rw_inode`? | Grep for `rw_inode` in `crates/fs/src/mfs/inode.rs` | 0s (grep) |
| 3 | Does the RPC reply contain the right bytes? | Add `serial_write` of `p_delivermsg` bytes 8–31 in boot test | 1 QEMU cycle |
| 4 | Does VFS parse the reply at the right offsets? | Check `Message` struct layout and `PAYLOAD_OFF` values | 0s (read code) |
| 5 | Does `mount_root` check the return value? | Read `mount_root()` in VFS | 0s (read code) |

## Boot Tests Are Your Fastest Probe

The `boot-test` feature (`just test-boot`) runs assertions inside the kernel before any shell starts. Use it to:

- **Check a function return**: `test_vfs_reply_from_mfs()` reads VFS's `p_delivermsg.m_type` to confirm OK
- **Dump raw bytes**: Add a `for i in 8..32 { print_hex(msg[i]); }` loop — you get concrete data, not speculation
- **Test one hypothesis per cycle**: Add one test, run it, see the result. Don't add 5 tests and wonder which failed
- **Remove it after**: Debug tests are temporary. Remove or gate them once the issue is resolved

### Boot Test Patterns

```rust
// Pattern 1: Check a return value / status code
fn test_something_worked() -> u32 {
    unsafe {
        let rp = kernel::table::proc_addr(SOME_PROC_NR);
        if (*rp).p_delivermsg.m_type != OK {
            serial_write("  FAIL: ...\r\n");
            return 1;
        }
    }
    0
}

// Pattern 2: Dump raw bytes (expensive — remove after debugging)
// serial_write("  DBG: bytes 8-15: ");
// for i in 8..16 { print_hex(*msg.add(i)); serial_write(" "); }
// serial_write("\r\n");

// Pattern 3: Compare two values you expect to match
if expected != actual {
    serial_write("  FAIL: expected "); print_dec(expected);
    serial_write(" got "); print_dec(actual); serial_write("\r\n");
    return 1;
}
```

## Avoid Tunnel Vision

When a symptom points to a complex subsystem (page tables, IPC, grants), **actively list 3–5 simpler explanations before investigating the complex one.**

Counter-example from this session:
- Symptom: `req_getdents` returns zeros
- Jumped to: `virtual_copy` / grant table / CR3 switching
- Written: multi-page Blocker 3 analysis
- Actual cause: `rw_inode` never called + builder didn't set inode fields
- Verification cost: one grep + one boot test

**Checklist before investigating any complex subsystem:**

- [ ] Is the function in question actually **called**? (grep for callsites)
- [ ] Does the function exist? (grep for its definition)
- [ ] If it reads data from disk/memory, is the data populated? (hexdump the source)
- [ ] If it involves IPC, is the reply being checked for errors?
- [ ] Are struct layouts compatible? (offsets, sizes, alignment)
- [ ] Are constants correct? (feature flags, endpoint numbers, message types)
- [ ] Is there a simpler path that bypasses the complex subsystem entirely?

## When You Find One Bug, Look for a Second

If fixing one thing doesn't fix the symptom, **don't escalate complexity** — look for another simple bug at the same layer. In this session, `rw_inode` was missing AND the builder didn't populate mode/size. Either alone caused the same symptom. 

## Quick Reference: Boot Test Infrastructure

```
just test-boot          # Build + boot QEMU + run 12 assertions + exit
just run                # Normal boot (skips test, starts shell)
```

Test file: `crates/kernel-boot/src/boot_test.rs`
Test runner: `pub unsafe fn run_boot_tests() -> !` in the same file.

The kernel uses `syscall 60` (`SYS_BOOT_COMPLETE`) as a signal from VFS to trigger the test suite. The handler is in `crates/kernel-boot/src/main.rs` as `boot_test_syscall_handler`.
