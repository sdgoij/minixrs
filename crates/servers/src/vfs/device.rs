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

// CDEV message field offsets (absolute byte offsets in a 56-byte Message:
// m_type @ 4, payload @ 8). The m2 fields are: m2i1 @ 8, m2i2 @ 12, m2i3 @ 16,
// m2l1 @ 24, m2l2 @ 32, m2l3 @ 40. These must match the tty server's
// handle_cdev_request reads (crates/servers/src/tty.rs).
const CDEV_MINOR_OFF: usize = 8; // m2_i1
const CDEV_FLAGS_OFF: usize = 12; // m2_i2
const CDEV_USER_OFF: usize = 16; // m2_i3
const CDEV_POS_OFF: usize = 24; // m2_l1
const CDEV_COUNT_OFF: usize = 32; // m2_l2
const CDEV_BUF_OFF: usize = 40; // m2_l3 (CDEV_WRITE inline data)

// Access flags for CDEV_OPEN — must match the tty server's CDEV_* constants
// (crates/servers/src/tty.rs), which differ from the C chardriver.h values.
const CDEV_R_BIT: i32 = 0x04;
const CDEV_W_BIT: i32 = 0x08;
const CDEV_NOCTTY: i32 = 0x02;

/// Reply flag: a successful CDEV_OPEN made the device the controlling tty.
const CDEV_CTTY: i32 = 2;

/// O_NOCTTY open flag (minix/include/fcntl.h).
const O_NOCTTY: i32 = 0o400;

/// Max bytes the tty replies inline per CDEV_READ (48-byte payload).
const INLINE_READ_MAX: usize = 48;
/// Max bytes the tty accepts inline per CDEV_WRITE (m2_l3 = last 8 payload bytes).
const INLINE_WRITE_MAX: usize = 8;

/// Convert open(2) flags to the tty server's CDEV access bits.
/// O_RDONLY=0, O_WRONLY=1, O_RDWR=2 (access mode in the low two bits).
fn open_access_flags(flags: i32) -> i32 {
    let mode = flags & 0o3;
    let mut access = 0;
    if mode != 1 {
        access |= CDEV_R_BIT;
    }
    if mode != 0 {
        access |= CDEV_W_BIT;
    }
    if flags & O_NOCTTY != 0 {
        access |= CDEV_NOCTTY;
    }
    access
}

/// Build a CDEV_OPEN/CDEV_CLOSE request message.
fn build_open_close_msg(op: i32, dev: u32, access: i32, user_ep: i32) -> [u8; 56] {
    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, op); // m_type
    request::w_i32(&mut msg, CDEV_MINOR_OFF, (dev & 0xFFFF) as i32);
    request::w_i32(&mut msg, CDEV_FLAGS_OFF, access);
    request::w_i32(&mut msg, CDEV_USER_OFF, user_ep);
    msg
}

// Character device operations

/// Open a character device.
///
/// Sends a \`CDEV_OPEN\` message to the device driver endpoint found via the
/// dmap table for the given \`dev\`'s major number. On a \`CDEV_CTTY\` reply
/// the device becomes the caller's controlling terminal.
///
/// C source: \`minix/servers/vfs/device.c\` — \`cdev_open()\` (line 484)
///
/// # Safety
///
/// Requires exclusive access to the global fproc/dmap tables.
pub unsafe fn cdev_open(dev: u32, flags: i32) -> i32 {
    let dp = dmap::get_dmap_by_major((dev >> 16) as i32);
    if dp.is_null() {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }

    // The user endpoint travels in m2_i3 so the driver can track the
    // controlling terminal and the read caller.
    let user_ep = unsafe { (*crate::vfs::glo::current_fp()).fp_endpoint };
    let mut msg = build_open_close_msg(CDEV_OPEN, dev, open_access_flags(flags), user_ep);

    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if r < 0 {
        return r;
    }
    // A CDEV_CTTY bit in the reply means the open made this device the
    // controlling terminal (C cdev_opcl: fp->fp_tty = dev).
    if r & CDEV_CTTY != 0 {
        unsafe { (*crate::vfs::glo::current_fp()).fp_tty = dev as i32 };
    }
    OK
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
pub unsafe fn cdev_close(dev: u32) -> i32 {
    let dp = dmap::get_dmap_by_major((dev >> 16) as i32);
    if dp.is_null() {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }
    let mut msg = build_open_close_msg(CDEV_CLOSE, dev, 0, 0);
    unsafe { request::fs_sendrec(drv_e, &mut msg) }
}

/// Perform I/O on a character device.
///
/// Builds a CDEV_READ/CDEV_WRITE/CDEV_IOCTL message and sends it to the
/// driver via synchronous sendrec. The driver endpoint is resolved from
/// the dmap table using the device's major number.
pub fn cdev_io(op: i32, dev: u32, proc_e: i32, buf: u64, pos: i64, bytes: u64, _flags: i32) -> i32 {
    let dp = dmap::get_dmap_by_major((dev >> 16) as i32);
    if dp.is_null() {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }
    let minor = (dev & 0xFFFF) as i32;

    if op == CDEV_WRITE {
        // Writes travel inline in m2_l3 (last 8 payload bytes); loop so
        // arbitrary user buffer lengths are written in full.
        let mut written: u64 = 0;
        let mut cur_pos = pos;
        while written < bytes {
            let chunk = ((bytes - written) as usize).min(INLINE_WRITE_MAX);
            let mut msg = [0u8; 56];
            request::w_i32(&mut msg, 4, CDEV_WRITE);
            request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
            request::w_i32(&mut msg, CDEV_FLAGS_OFF, 0);
            request::w_i32(&mut msg, CDEV_USER_OFF, proc_e);
            request::w_i64(&mut msg, CDEV_POS_OFF, cur_pos);
            request::w_u64(&mut msg, CDEV_COUNT_OFF, chunk as u64);
            let copy_r = unsafe {
                crate::vfs::call::sys_vircopy(
                    proc_e,
                    buf + written,
                    crate::vfs::call::SELF,
                    msg.as_mut_ptr() as u64 + CDEV_BUF_OFF as u64,
                    chunk,
                )
            };
            if copy_r != 0 {
                return if written > 0 { written as i32 } else { copy_r };
            }
            let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
            if r < 0 {
                return if written > 0 { written as i32 } else { r };
            }
            written += chunk as u64;
            cur_pos += chunk as i64;
        }
        return written as i32;
    }

    if op == CDEV_READ {
        // The tty replies with data inline in the payload; request at most
        // what the 48-byte payload can carry per round trip. A short read is
        // fine (blocking reads return when a line/queue is ready).
        let want = (bytes as usize).min(INLINE_READ_MAX);
        let mut msg = [0u8; 56];
        request::w_i32(&mut msg, 4, CDEV_READ);
        request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
        request::w_i32(&mut msg, CDEV_FLAGS_OFF, 0);
        request::w_i32(&mut msg, CDEV_USER_OFF, proc_e);
        request::w_i64(&mut msg, CDEV_POS_OFF, pos);
        request::w_u64(&mut msg, CDEV_COUNT_OFF, want as u64);
        let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
        if r < 0 {
            return r;
        }
        let n = r as usize;
        if n > 0 && buf != 0 {
            let copy_r = unsafe {
                crate::vfs::call::sys_vircopy(
                    crate::vfs::call::SELF,
                    msg.as_ptr() as u64 + 8, // reply data at payload[0..]
                    proc_e,
                    buf,
                    n,
                )
            };
            if copy_r != 0 {
                return copy_r;
            }
        }
        return r;
    }

    // CDEV_IOCTL: request code in m2_i2, reply status only (grant data path
    // is not wired).
    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, op);
    request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
    request::w_i32(&mut msg, CDEV_FLAGS_OFF, bytes as i32); // ioctl request
    request::w_i32(&mut msg, CDEV_USER_OFF, proc_e);
    unsafe { request::fs_sendrec(drv_e, &mut msg) }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_access_flags_rdonly() {
        // O_RDONLY = 0: read access only.
        assert_eq!(open_access_flags(0o00), CDEV_R_BIT);
    }

    #[test]
    fn test_open_access_flags_wronly() {
        // O_WRONLY = 1: write access only.
        assert_eq!(open_access_flags(0o01), CDEV_W_BIT);
    }

    #[test]
    fn test_open_access_flags_rdwr() {
        // O_RDWR = 2: read + write access.
        assert_eq!(open_access_flags(0o02), CDEV_R_BIT | CDEV_W_BIT);
    }

    #[test]
    fn test_open_access_flags_noctty() {
        // O_RDWR | O_NOCTTY: read + write, not the controlling tty.
        assert_eq!(
            open_access_flags(0o02 | 0o400),
            CDEV_R_BIT | CDEV_W_BIT | CDEV_NOCTTY
        );
    }

    #[test]
    fn test_open_close_msg_layout() {
        // The tty server reads m2_i1 (minor @8), m2_i2 (access @12),
        // m2_i3 (user endpoint @16) and m_type @4. Verify our builder
        // matches that contract.
        let dev = (5u32 << 16) | 0; // major 5 (console), minor 0
        let msg = build_open_close_msg(CDEV_OPEN, dev, CDEV_R_BIT | CDEV_W_BIT, 42);
        assert_eq!(request::r_i32(&msg, 4), CDEV_OPEN);
        assert_eq!(request::r_i32(&msg, CDEV_MINOR_OFF), 0);
        assert_eq!(
            request::r_i32(&msg, CDEV_FLAGS_OFF),
            CDEV_R_BIT | CDEV_W_BIT
        );
        assert_eq!(request::r_i32(&msg, CDEV_USER_OFF), 42);

        let msg = build_open_close_msg(CDEV_CLOSE, dev, 0, 0);
        assert_eq!(request::r_i32(&msg, 4), CDEV_CLOSE);
        assert_eq!(request::r_i32(&msg, CDEV_MINOR_OFF), 0);
    }

    #[test]
    fn test_offset_constants_match_tty_contract() {
        // The tty server reads minor/flags/user at these payload offsets.
        assert_eq!(CDEV_MINOR_OFF, 8);
        assert_eq!(CDEV_FLAGS_OFF, 12);
        assert_eq!(CDEV_USER_OFF, 16);
        assert_eq!(CDEV_POS_OFF, 24);
        assert_eq!(CDEV_COUNT_OFF, 32);
        assert_eq!(CDEV_BUF_OFF, 40);
    }
}
