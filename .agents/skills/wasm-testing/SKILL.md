---
name: wasm-testing
description: Building and testing the wasm32-minix system (kernel, servers, program module) and keeping its two harness process lists in step with the shared boot image and VFS's mounts. Use when changing BOOT_IMAGE or BOOT_PROCS_ALL, adding or renaming a boot process or a *PROC_NR, changing boot-image manifests, adding a mount that VFS performs during init, touching crates/wasm-*, tools/wasm-*, or when a wasm CI job fails.
---

# Testing a wasm build

The wasm port runs the *real* servers, compiled for `wasm32-minix`, under a host loader. It is
the port's strongest surface and it is easy to forget: none of it is compiled by `cargo test`,
and `just check` does not link it. Any change that alters which processes exist, or which peers
VFS waits on at boot, can break it with every host gate green.

## The rule

A change is not done until one of these is true:

- the wasm gate was run and passed **before the commit** (`sh tools/wasm-verify.sh`), or
- the change cannot reach wasm, or
- the wasm gate could not be run on this host, and the hand-off says so by name and points at CI's
  `wasm system` job as the remaining check. Never claim wasm is green without one of the first two.

## Why it drifts: two process lists and one shared boot image

`crates/kernel/src/table.rs`'s `BOOT_IMAGE` is shared by every target.
`crates/servers/src/vfs/main.rs` runs unchanged on wasm, so every mount in `sef_cb_init_fresh`
happens there too, blocking on the endpoint it sends to. But the wasm system's instances are a
*second* statement of what exists:

| File | What it is |
|---|---|
| `crates/wasm-servers/src/lib.rs` | one `minix_server_<name>` export per server (and the DS clients, `minix_init`) |
| `crates/wasm-program/src/lib.rs` | `minix_program_main`, the program module |
| `tools/wasm-servers/boot.cjs` | the check harness's `specs`: slot, entry, extra modules |
| `tools/wasm-browser/host.js` | the page's `SYSTEM_SPECS`: the same list minus the test-only slots |

The hardware arches load every `BOOT_PROCS_ALL` entry from the initramfs; the wasm host spawns
only what its specs name. When a boot process VFS or another server talks to has no instance,
the guest boots part-way and hangs on a peer that will never answer.

## Worked example: the `/proc` mount broke the wasm boot

Adding procfs as boot process 21 and `mount_procfs` in VFS's init was correct on the hardware
arches and green locally (`just test-arches`). CI's `wasm system` job failed with five `FAIL`
lines, the first being `every server reached its main loop` with `vfs: 50,48,48,48` (four
syscalls: the root mount, then the procfs `REQ_READSUPER`, then nothing). VFS was blocked in
`mount_procfs` because slot 21 had no procfs instance, the harness having given it to its
unregistered DS test client, so it never reached `mount_devman`.

The fix was to run procfs on wasm too: export `minix_server_procfs` over
`fs::procfs::main::procfs_main`, add `{ slot: 21, entry: 'minix_server_procfs' }` to both spec
lists before VFS, and move the test client to slot 24. Running procfs on wasm is the right shape:
`BOOT_IMAGE` names it on every target and VFS mounts it unconditionally, so the alternative (a
`cfg` that skips the mount on wasm) would be a divergence in server code the project avoids.

This failure is recorded here rather than as a `KNOWN_ISSUES` item because it is a workflow gap,
not an open defect: the mount was correct, the harness was not updated with it.

## The wasm gate

This is the gate. Run it with `sh tools/wasm-verify.sh` on the checkout you are committing, and
do not commit if it fails.

```sh
sh tools/wasm-verify.sh            # all three harnesses
sh tools/wasm-verify.sh boot       # tools/wasm-servers/boot.cjs: quiescence, every device
sh tools/wasm-verify.sh run        # tools/wasm-browser/run.js: a shell at a live prompt
sh tools/wasm-verify.sh page       # tools/wasm-browser/page.test.js: the page under a stub DOM
```

It builds the three workspaces (`kernel-wasm`, `wasm-servers`, `wasm-program`), applies Binaryen's
Asyncify to the servers and the program, builds the wasm boot image, then runs the harnesses.

Requirements:

- **A stage1 whose sysroot has the `wasm32-minix` target, and a wasm-capable linker.** On Windows
  that linker is the checkout's ci-llvm `lld.exe`, which `tools/lld.py` resolves (the stage1 ships
  no rust-lld). To use a stage1 from another host, set `MINIXRS_RUSTC` and `MINIXRS_WASM_LINKER`
  (`MINIXRS_STAGE1_DEST=/tmp/stage1 python tools/fetch-stage1.py`).
- **Binaryen's `wasm-opt`** at `tools/fork-spike/.tools/node_modules/binaryen/bin/wasm-opt`
  (`npm install binaryen --prefix tools/fork-spike/.tools --no-save`).
- **Node** (the harnesses are `.cjs`/ES modules) and `python` on `PATH`.

## Reading a wasm failure

`boot.cjs` prints one `PASS`/`FAIL` line per property with the measured values underneath. The
signatures worth knowing:

- **Every server fails `reached its main loop`, and VFS's trace is a short prefix.** VFS is blocked
  on an absent peer during init. Find the mount it stopped at (the last syscall) and give that
  endpoint an instance. This is the `/proc` failure above.
- **`the kernel accepts every declared slot into its image table` fails.** A spec's slot cannot be
  added to the image table; check `minix_image_add` and that the slot is not already taken by a
  different process.
- **`typeof exports[entry]` is not a function.** The spec names an entry the module does not export;
  check both spec lists against `crates/wasm-servers` and `crates/wasm-program`, and add the shim.
- `WASM_TRACE=1` / `WASM_TRACE_IPC=1` print the syscall traces the checks read.

A gate that never spoke has failed, even if the step exits 0: the bootstrap for `wasm-verify.sh`
is `set -eu`, and a timeout (`WASM_TIMEOUT`, default 600 s) is a verdict, not a reason to wait
longer.

## Slots and test-only processes

Boot process numbers come from `crates/arch-common/src/com.rs` (`0` PM through `21` procfs). A
harness spec at one of those numbers must be that process. Above them, `boot.cjs` also spawns
test-only processes: slot 22 the program module, 23 a second program, 24 the unregistered DS
client. Any new boot process needs its harness slot checked against those; the client moved off
21 when procfs took it.

## Publishing

The browser demo is staged from `main` into `docs/` (`just publish-wasm` /
`tools/wasm-browser/publish.sh`), and the release gate does not publish unless `wasm system`
passed. A wasm break therefore stops the demo, not just a test.
