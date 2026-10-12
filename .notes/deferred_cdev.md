# Deferred character-device I/O — driver migration

Status snapshot: 2026-10-09, after `06bd70f45` (`fix(vfs,tty): gate deferred cdev I/O to the tty`).

This tracks the work left to make VFS's character-device path match the
reference: **every** driver answers READ/WRITE/IOCTL asynchronously, and VFS
always suspends the caller rather than blocking itself. Today only `tty` does;
the rest still answer inline from a synchronous `fs_sendrec`, and a
`dmap_cdev_async` flag keeps VFS honest about which is which.

Nothing here is required for `v0.1.0-preview.2`. It is cleanup plus the one
latent bug the flag papers over (see "Why", below).

---

## Why this exists

`70fbb3ff8` changed VFS to hand a request to a driver with
`asynsend3(AMF_NOREPLY)` and suspend the caller, waiting for a `CDEV_REPLY`
(the reference's `cdev_io` / `cdev_generic_reply`, and the tty's `tty.c`
`do_read`). It did this for **every** character device but converted only the
tty driver.

Every other driver still replies the old way (`msg.m_type = result; SEND`), so
VFS reads that plain reply as a caller request, cannot match it to the
suspended caller, and the caller hangs. This is not wasm-specific: it is
latent on all three hardware arches too, and was escaped in CI only because no
hardware gate does a **non-datagram** char read to a non-tty driver
(`gltriangle` opens no DRM node; the wserver's `/dev/fb` frame writes are
*dgram*, which bypasses the deferral). The wasm gate is the one that runs
`ping`, which reads/writes `/dev/ip`.

`06bd70f45` gated the deferral behind `dmap_cdev_async` (set for the tty
majors only) and restored the synchronous path for everything else. That
fixed wasm and left the hardware arches untouched, but the flag is a
temporary fork of the protocol.

`KNOWN_ISSUES.md` item 7 (the tty chardriver item) is the prior note; this
document supersedes its "`cdev_reply` async-reply dispatch is still a stub"
line for the `CDEV_REPLY` arm.

---

## The protocol, as implemented

VFS side — `crates/servers/src/vfs/device.rs`:

- `cdev_io` takes the deferred path for a driver whose dmap entry has
  `dmap_cdev_async`; otherwise it stays on the synchronous `fs_sendrec`.
  Datagram I/O is synchronous either way.
- `cdev_insert` records the request in a fixed table (`CDEV_WAITS`,
  `MAX_CDEV_WAITS = 16`) and marks the caller `FP_BLOCKED_ON_OTHER`; the
  request's **caller endpoint** (`w.ep`) is the key.
- `cdev_asynsend` sends with `AMF_NOREPLY`; `cdev_io_reply` matches the
  driver's reply to the wait by that endpoint and completes it with
  `cdev_reply_send` (a plain `SEND` back to the caller).

Driver side — template in `crates/servers/src/tty.rs`:

- `handle_cdev_request` returns `EDONTREPLY` when it must wait; the main loop
  sends nothing for that result.
- On completion, `chardriver_reply_task_data(endpt, id, status, data)` (or
  `chardriver_reply_task` with no data) builds a `CDEV_REPLY` and sends it
  with a **blocking** `SEND`.

`CDEV_REPLY` layout (the 48-byte payload; message bytes shown):

| bytes | field |
|---|---|
| `[8..12]` | status (the read count, 0, or `-errno`) |
| `[12..16]` | `id` — the caller endpoint the driver echoes back |
| `[16..56]` | inline read data, at most `CDEV_REPLY_DATA_MAX = 40` |

The driver reads `id` out of the **request**: `m2_i3` (byte 16) for READ and
WRITE, `m2_l1` (byte 24) for IOCTL.

---

## Tasks

Each task: convert the driver's main loop to answer `CDEV_REPLY`, read the
caller endpoint from the request, then add its majors to `mark_cdev_async`.
Land them one at a time; each is independently gated.

- [ ] **`net` — major `NET_MAJOR` (14), `/dev/ip`.** The driver that failed on
      wasm. `net.rs` main loop (`~2900`) replies inline; `cdev_read` writes the
      IP datagram inline at payload 0 (its own comment says so), `cdev_write`
      takes an inline 8-byte request. Convert both; the deferred READ request
      caps `count` at 40 bytes (`cdev_io`), so a datagram up to 40 bytes fits —
      check the largest `/dev/ip` reply stays inside it. Its internal
      `dl_read_frames` poll no longer holds VFS once deferred, which is the
      point.
- [ ] **`fb` — major `FB_MAJOR` (19) and the DRM render node `DRM_MAJOR` (145).**
      Both served by `fb.rs`. IOCTL is the live path (`FBIOFLUSH`, the DRM
      ioctls behind `handle_drm_request`); non-datagram READ/WRITE too, though
      the wserver's frame writes are datagram and already synchronous. Establish
      whether any DRM ioctl's reply carries payload and, if so, which offset.
- [ ] **`input` — major `INPUT_MAJOR` (20), `/dev/kbd`.** `input.rs` main loop
      (`~570`); CDEV_READ returns 8-byte event records inline.
- [ ] **`uds` — major `UDS_MAJOR` (18), `/dev/uds`.** `uds.rs` main loop
      (`~958`). **Trap:** `vfs::device::cdev_ioctl_payload` (used by `vfs::scm`
      for fd passing) sends a `GRANT_INVALID` `CDEV_IOCTL` with a **synchronous**
      `fs_sendrec` and reads three payload words out of the reply. If uds answers
      `CDEV_REPLY` unconditionally, that helper must be taught the new layout —
      or those ioctls routed through `cdev_io`. The two socket control ioctls are
      already handled in `do_ioctl` before `cdev_io`, so only this helper is
      affected.
- [ ] **Shared reply helper.** `chardriver_reply_task[_data]` currently lives in
      `tty.rs`. Move it to a shared module (e.g. `crates/minix-util/src/`) so the
      four drivers above do not each grow a copy.

## Endgame

Once every driver is converted:

- [ ] Delete `dmap_cdev_async` (`vfs/types.rs`), `dmap::mark_cdev_async`
      (`vfs/dmap.rs`), the two `mark_cdev_async` calls (`vfs/main.rs`), and the
      `if !async_cdev` branches in `cdev_io` — leaving the reference's uniform
      deferral.
- [ ] Re-check `dmap_unmap_by_endpt` (driver restart): it clears the flag today;
      with the flag gone there is nothing to keep in step.

---

## Related, but separate: the wasm console pump

Not part of the driver migration. `06bd70f45` left a wasm-only poll in
`tty::do_read` (`#[cfg(target_arch = "wasm32")]`), because this target has no
clock to fire the `SYS_SETALARM` that drives the tty's console pump, and the
spin *does* yield on wasm (the host reschedules at `thread_yield`) rather than
starving VFS as it did on the hardware arches.

- [ ] Optional: give the wasm kernel a host-input notification (the counterpart
      of the UART ISR that wakes the console driver on the real arches), so the
      suspend model can replace the wasm spin too. Until then the spin stays.

---

## Verification

Every task lands with the same evidence the tty conversion used.

- Wasm (Fedora WSL + the fetched Linux stage1): `boot.cjs`, `run.js`,
  `page.test.js` — `run.js` and `page.test.js` are what exercise `ping`, so
  `net` in particular must be checked there.
- Hardware: `test-gltriangle-{x86,riscv64,aarch64}` (console + DRM path),
  `test-interactive-bash-{arch}` (console read/write), `test-boot-{arch}`.
- Host: `cargo clippy --all-targets -- -D warnings` and `cargo test -p servers`
  (743 tests today).

## Traps

- **Data size.** The deferred reply carries at most 40 bytes of inline data;
  the old synchronous path carried 48. A driver whose READ returns more than 40
  must be checked (and the request's `count` capped) rather than truncated
  silently.
- **Blocking send.** The reply must use `SEND`, not `SENDNB`: a non-blocking
  reply is refused while VFS is busy and is lost, and the caller waits forever.
  Same for the tty.
- **The `id` field.** Wrong offset (read/write `m2_i3` vs ioctl `m2_l1`) means
  `cdev_io_reply` matches the wrong wait, or none.
- **Match key.** `cdev_io_reply` matches on the caller endpoint alone. That is
  unique while a caller has one request in flight (a blocked single-threaded
  reader), but converting more drivers makes it worth confirming no process can
  have two deferred requests outstanding — if one can, the key needs the driver
  too.
- **Restart.** `dmap_unmap_by_endpt` clears `dmap_cdev_async`; any conversion
  must stay consistent if RS restart for a driver is ever wired.
