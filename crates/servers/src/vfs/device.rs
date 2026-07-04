//! Character device (\`cdev_*\`) and block device (\`bdev_*\`) operations.
//!
//! Adapted from \`minix/servers/vfs/device.c\`.
//!
//! The functions in this module perform device I/O by sending/receiving
//! IPC messages to registered device driver processes identified via the
//! device mapping (dmap) table.

use crate::vfs::consts::*;
use crate::vfs::dmap;
use crate::vfs::request;
use crate::vfs::types::*;

// Character device operations

/// Open a character device.
///
/// Sends a \`CDEV_OPEN\` message to the device driver endpoint found via the
/// dmap table for the given \`dev\`'s major number.  If the device is cloned
/// the minor number may be replaced.
///
/// C source: \`minix/servers/vfs/device.c\` — \`cdev_open()\` (line 484)
///
/// # Safety
///
/// Requires exclusive access to the global fproc/dmap tables.
///
/// # TODO
///
/// Wire IPC send/recv to the character driver endpoint.  The underlying
/// \`cdev_opcl()\` helper performs:
///   1. Lookup dmap entry via \`cdev_get()\`.
///   2. Build \`CDEV_OPEN\` message with minor, flags, access bits.
///   3. \`asynsend3()\` + \`worker_wait()\` to block until reply.
///   4. Handle \`CDEV_CLONED\` / \`CDEV_CTTY\` flags in the reply.
pub fn cdev_open(_dev: u32, _flags: i32) -> i32 {
    ENOSYS
}

/// Close a character device.
///
/// Sends a `CDEV_CLOSE` message to the device driver.
///
/// C source: `minix/servers/vfs/device.c` — `cdev_close()` (line 495)
///
/// # Safety
///
/// Requires exclusive access to the global fproc/dmap tables.
///
/// # TODO
///
/// Wire IPC send/recv to the character driver endpoint.  The underlying
/// `cdev_opcl()` helper mirrors cdev_open's flow with `CDEV_CLOSE`.
pub fn cdev_close(_dev: u32) -> i32 {
    ENOSYS
}

/// Perform I/O on a character device.
///
/// Builds a CDEV_READ/CDEV_WRITE/CDEV_IOCTL message and sends it to the
/// driver via synchronous sendrec. The driver endpoint is resolved from
/// the dmap table using the device's major number.
pub fn cdev_io(op: i32, dev: u32, proc_e: i32, buf: u64, pos: i64, bytes: u64, _flags: i32) -> i32 {
    let major = (dev >> 16) as i32;
    let minor = dev & 0xFFFF;
    let dp = dmap::get_dmap_by_major(major);
    if dp.is_null() {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }

    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, minor as i32); // m2_i1
    request::w_i32(&mut msg, 8, proc_e); // m2_i2
    request::w_i32(&mut msg, 12, -1); // m2_i3 = grant (stub)
    request::w_i64(&mut msg, 16, pos); // m2_l1 = position
    request::w_u64(&mut msg, 24, bytes); // m2_l2 = count/request
    request::w_u64(&mut msg, 32, buf); // m2_l3 = user buffer
    request::w_i32(&mut msg, 0, op); // m_type

    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if r != 0 {
        return r;
    }
    // Reply status in m_type, bytes in m2_l1
    request::r_i64(&msg, 16) as i32
}

/// Map a character device to a different device number.
///
/// Handles the \`/dev/tty\` special case (\`CTTY_MAJOR\`): when the given
/// device is the controlling-tty major, it is remapped to the process's
/// actual controlling terminal device stored in \`rfp.fp_tty\`.
///
/// C source: \`minix/servers/vfs/device.c\` — \`cdev_map()\` (line 205)
///
/// # Safety
///
/// Requires the caller to hold a valid reference to \`rfp\`.
///
/// # TODO
///
/// When \`CTTY_MAJOR\` support is wired, check \`rfp.fp_tty\` and substitute
/// the controlling terminal device.  Perform bounds checking on the major
/// number against \`NR_DEVICES\`.
pub fn cdev_map(dev: u32, rfp: *const Fproc) -> u32 {
    let _ = rfp;
    dev
}

/// Initiate a select call on a character device.
///
/// Sends a CDEV_SELECT message to the driver via synchronous sendrec.
pub fn cdev_select(dev: u32, ops: i32) -> i32 {
    let major = (dev >> 16) as i32;
    let minor = dev & 0xFFFF;
    let dp = dmap::get_dmap_by_major(major);
    if dp.is_null() {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }

    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, minor as i32);
    request::w_i32(&mut msg, 8, ops);
    request::w_i32(&mut msg, 0, CDEV_SELECT);

    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if r != 0 {
        return r;
    }
    // Reply: selected ops in m2_i1
    request::r_i32(&msg, 4)
}

/// Cancel an I/O request on a character device.
///
/// Sends a \`CDEV_CANCEL\` message to the driver, then blocks until the
/// cancellation is confirmed.  Any outstanding grant for the request's
/// buffer is revoked.
///
/// C source: \`minix/servers/vfs/device.c\` — \`cdev_cancel()\` (line 586)
///
/// # Safety
///
/// Requires exclusive access to the global fproc/dmap tables.
///
/// # TODO
///
/// Wire the full flow:
///   1. Resolve dmap via \`cdev_get()\`.
///   2. Build \`CDEV_CANCEL\` message with minor and caller endpoint.
///   3. \`asynsend3()\` then \`worker_wait()\`.
///   4. Revoke the grant (\`cpf_revoke()\`) on completion.
///   5. Convert \`EAGAIN\` to \`EINTR\` per protocol convention.
pub fn cdev_cancel(dev: u32) -> i32 {
    let _ = dev;
    ENOSYS
}

/// Process the result of a character driver request.
///
/// Dispatches incoming character driver replies to the appropriate handler:
///
/// * \`CDEV_REPLY\` — open/close/read/write/ioctl result → \`cdev_generic_reply()\`.
/// * \`CDEV_SEL1_REPLY\` — first select reply → \`select_reply1()\`.
/// * \`CDEV_SEL2_REPLY\` — second select reply → \`select_reply2()\`.
///
/// C source: \`minix/servers/vfs/device.c\` — \`cdev_reply()\` (line 794)
///
/// # Safety
///
/// Must be called from the VFS main loop when a \`CDEV_REPLY\`,
/// \`CDEV_SEL1_REPLY\`, or \`CDEV_SEL2_REPLY\` message is received.
///
/// # TODO
///
/// Wire reply dispatch: validate the driver endpoint via \`get_dmap()\`,
/// then switch on the incoming call number and call the appropriate reply
/// handler.
pub fn cdev_reply() {
    // TODO: read call_nr from global state, dispatch to cdev_generic_reply,
    // select_reply1, or select_reply2.
}

// Block device operations

/// Open a block device.
///
/// Sends a \`BDEV_OPEN\` message to the block driver, requesting access
/// according to the \`access\` flags (\`R_BIT\` / \`W_BIT\`).
///
/// C source: \`minix/servers/vfs/device.c\` — \`bdev_open()\` (line 44)
///
/// # Safety
///
/// Requires exclusive access to the global dmap table.
///
/// # TODO
///
/// Wire IPC:
///   1. Lookup driver via \`dmap[major_dev]\`.
///   2. Build \`BDEV_OPEN\` message with minor and access bits.
///   3. Call \`block_io()\` (synchronous send/recv wrapper).
///   4. Return the status from the driver reply.
pub fn bdev_open(dev: u32, access: i32) -> i32 {
    let _ = (dev, access);
    ENOSYS
}

/// Close a block device.
///
/// Sends a \`BDEV_CLOSE\` message to the block driver.
///
/// C source: \`minix/servers/vfs/device.c\` — \`bdev_close()\` (line 77)
///
/// # Safety
///
/// Requires exclusive access to the global dmap table.
///
/// # TODO
///
/// Wire IPC via \`block_io()\`: build \`BDEV_CLOSE\` message and send it
/// synchronously to the driver.
pub fn bdev_close(dev: u32) -> i32 {
    let _ = dev;
    ENOSYS
}

/// Process the result of a block driver request.
///
/// Wakes up the worker thread that is waiting for a block driver reply.
/// The reply message is copied into the worker's sendrec buffer.
///
/// C source: \`minix/servers/vfs/device.c\` — \`bdev_reply()\` (line 824)
///
/// # Safety
///
/// Must be called from the VFS main loop when a \`BDEV_REPLY\` message
/// is received.
///
/// # TODO
///
/// Wire reply processing:
///   1. Validate driver via \`get_dmap()\`.
///   2. Lookup the servicing worker thread from \`dmap_servicing\`.
///   3. Copy the incoming message into \`w_drv_sendrec\`.
///   4. Signal the worker thread with \`worker_signal()\`.
pub fn bdev_reply() {
    // TODO: lookup driver endpoint, copy reply message into worker's
    // sendrec buffer, and signal the waiting worker thread.
}

/// A block driver has been mapped in.
///
/// Reopens all block-special files that were previously opened on the
/// affected major device, and tells each mounted filesystem about the
/// new driver endpoint via \`req_newdriver()\`.
///
/// C source: \`minix/servers/vfs/device.c\` — \`bdev_up()\` (line 681)
///
/// # Safety
///
/// Requires exclusive access to the global filp, vmnt, and dmap tables.
///
/// # TODO
///
/// Wire the recovery flow:
///   1. Scan the filp table for block-special files matching \`major\`.
///   2. Call \`bdev_open()\` on each to re-establish the driver connection.
///   3. Scan the vmnt table for mounted filesystems on this major and
///      call \`req_newdriver()\` with the driver label.
///   4. If any block-special file was open, also notify the root FS.
pub fn bdev_up(major: i32) {
    let _ = major;
    // TODO: reopen block-special files and notify mounted filesystems.
}
