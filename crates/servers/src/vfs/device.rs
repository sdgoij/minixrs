//! Character device (\`cdev_*\`) and block device (\`bdev_*\`) operations.
//!
//! Adapted from \`minix/servers/vfs/device.c\`.
//!
//! The functions in this module perform device I/O by sending/receiving
//! IPC messages to registered device driver processes identified via the
//! device mapping (dmap) table.

use crate::vfs::consts::*;
use crate::vfs::dmap;
use crate::vfs::glo::vfs_global;
use crate::vfs::request;
use crate::vfs::types::*;

use core::ptr::addr_of_mut;

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

/// Max bytes an ioctl arg struct travels in the CDEV_IOCTL data area
/// (absolute bytes 24..56 of the message: m2_l1/m2_l2/m2_l3, unused by
/// ioctls). Socket option structs (nwio_udpopt_t = 16 bytes) fit easily.
const IOCTL_DATA_MAX: usize = 32;

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
/// Sends a `CDEV_OPEN` message to the device driver endpoint found via the
/// dmap table for the given `dev`'s major number. On a `CDEV_CTTY` reply
/// the device becomes the caller's controlling terminal.
///
/// Returns the device number to use for subsequent I/O on this filp — the
/// input `dev` for ordinary devices, or `(major << 16) | new_minor` when the
/// driver replied `CDEV_CLONED` (sockets allocate a fresh minor per open).
/// `reply_flags` receives the `CDEV_CTTY` / `CDEV_DGRAM_OPEN` reply bits.
///
/// C source: `minix/servers/vfs/device.c` — `cdev_open()` (line 484)
///
/// # Safety
///
/// Requires exclusive access to the global fproc/dmap tables.
pub unsafe fn cdev_open(dev: u32, flags: i32, reply_flags: *mut u32) -> i32 {
    let dp = dmap::get_dmap_by_major((dev >> 16) as i32);
    if dp.is_null() {
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
    let mut flags_out = 0u32;
    let mut out_dev = dev;
    if (r as u32) & CDEV_CLONED != 0 {
        // Socket drivers allocate a fresh minor per open; the filp must
        // use it for all subsequent I/O (C: cdev_clone swaps the vnode).
        // Clone minors live in the low bits, so skip the CTTY check (bit 1
        // of a clone reply is part of the minor, not a CTTY flag).
        let new_minor = r & 0xFFFF;
        out_dev = (dev & 0xFFFF_0000) | new_minor as u32;
        if (r as u32) & CDEV_DGRAM_OPEN != 0 {
            flags_out |= CDEV_DGRAM_OPEN;
        }
    } else if (r as u32) & CDEV_CTTY as u32 != 0 {
        // A CDEV_CTTY bit in the reply means the open made this device the
        // controlling terminal (C cdev_opcl: fp->fp_tty = dev).
        unsafe { (*crate::vfs::glo::current_fp()).fp_tty = dev as i32 };
        flags_out |= CDEV_CTTY as u32;
    }
    unsafe { *reply_flags = flags_out };
    out_dev as i32
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

    // Datagram (socket) devices: the whole buffer is one unit and travels
    // by vircopy — the driver copies user bytes itself using the user
    // endpoint (m2_i3) and VA (m2_l1). `flags` carries CDEV_DGRAM.
    let dgram = _flags & CDEV_DGRAM as i32 != 0;

    if op == CDEV_WRITE {
        if dgram {
            // One message per datagram: user VA in m2_l1, full length in m2_l2.
            let mut msg = [0u8; 56];
            request::w_i32(&mut msg, 4, CDEV_WRITE);
            request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
            request::w_i32(&mut msg, CDEV_FLAGS_OFF, CDEV_DGRAM as i32);
            request::w_i32(&mut msg, CDEV_USER_OFF, proc_e);
            request::w_i64(&mut msg, CDEV_POS_OFF, buf as i64);
            request::w_u64(&mut msg, CDEV_COUNT_OFF, bytes);
            return unsafe { request::fs_sendrec(drv_e, &mut msg) };
        }
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
        if dgram {
            // One message per datagram: user VA in m2_l1, max bytes in m2_l2.
            // The driver vircopy's the packet into the user buffer; the reply
            // status is the number of bytes copied.
            let mut msg = [0u8; 56];
            request::w_i32(&mut msg, 4, CDEV_READ);
            request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
            request::w_i32(&mut msg, CDEV_FLAGS_OFF, CDEV_DGRAM as i32);
            request::w_i32(&mut msg, CDEV_USER_OFF, proc_e);
            request::w_i64(&mut msg, CDEV_POS_OFF, buf as i64);
            request::w_u64(&mut msg, CDEV_COUNT_OFF, bytes);
            return unsafe { request::fs_sendrec(drv_e, &mut msg) };
        }
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

    // CDEV_IOCTL: request code in m2_i2; the arg struct travels in the
    // m2_l1/m2_l2/m2_l3 data area (absolute 24..56), sized by the ioctl's
    // NetBSD _IOC size field. _IOR ioctls get the struct copied back.
    let request = bytes as u32;
    let data_len = net::ioc_size(request).min(IOCTL_DATA_MAX);
    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, op);
    request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
    request::w_i32(&mut msg, CDEV_FLAGS_OFF, request as i32);
    request::w_i32(&mut msg, CDEV_USER_OFF, proc_e);
    if data_len > 0 {
        let copy_r = unsafe {
            crate::vfs::call::sys_vircopy(
                proc_e,
                buf,
                crate::vfs::call::SELF,
                msg.as_mut_ptr() as u64 + CDEV_POS_OFF as u64,
                data_len,
            )
        };
        if copy_r != 0 {
            return copy_r;
        }
    }
    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if r >= 0 && data_len > 0 && net::ioc_is_out(request) {
        let copy_r = unsafe {
            crate::vfs::call::sys_vircopy(
                crate::vfs::call::SELF,
                msg.as_ptr() as u64 + CDEV_POS_OFF as u64,
                proc_e,
                buf,
                data_len,
            )
        };
        if copy_r != 0 {
            return copy_r;
        }
    }
    r
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

// BDEV message field offsets (absolute byte offsets in the 56-byte
// Message, matching the block driver servers and minix-util/bdev.rs).
const BDEV_MINOR_OFF: usize = 8; // m2_i1
const BDEV_FLAGS_OFF: usize = 12; // m2_i2
const BDEV_STATUS_OFF: usize = 24; // m2_l2 (reply status)

// Access bits for BDEV_OPEN (minix/include/minix/bdev.h).
const BDEV_R_BIT: i32 = 0x01;
const BDEV_W_BIT: i32 = 0x02;

/// Open a block device.
///
/// Sends a `BDEV_OPEN` message to the block driver, requesting access
/// according to the `access` flags (`BDEV_R_BIT` / `BDEV_W_BIT`). The
/// driver endpoint comes from the dmap table for `dev`'s major number.
/// Returns the driver's status (0 on success, negative errno otherwise).
///
/// C source: `minix/servers/vfs/device.c` — `bdev_open()` (line 44)
pub fn bdev_open(dev: u32, access: i32) -> i32 {
    let dp = dmap::get_dmap_by_major((dev >> 16) as i32);
    if dp.is_null() {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }
    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, arch_common::com::BDEV_OPEN as i32);
    request::w_i32(&mut msg, BDEV_MINOR_OFF, (dev & 0xFFFF) as i32);
    request::w_i32(&mut msg, BDEV_FLAGS_OFF, access);
    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if r < 0 {
        return r;
    }
    // The driver replies with m_type = BDEV_REPLY and the status in
    // m2_l2 (absolute offset 24).
    request::r_i32(&msg, BDEV_STATUS_OFF)
}

/// Close a block device.
///
/// Sends a `BDEV_CLOSE` message to the block driver.
///
/// C source: `minix/servers/vfs/device.c` — `bdev_close()` (line 77)
pub fn bdev_close(dev: u32) -> i32 {
    let dp = dmap::get_dmap_by_major((dev >> 16) as i32);
    if dp.is_null() {
        return ENXIO;
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return ENXIO;
    }
    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, arch_common::com::BDEV_CLOSE as i32);
    request::w_i32(&mut msg, BDEV_MINOR_OFF, (dev & 0xFFFF) as i32);
    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if r < 0 {
        return r;
    }
    request::r_i32(&msg, BDEV_STATUS_OFF)
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

/// A block driver has been mapped in (or restarted).
///
/// Reopens all block-special files that were previously opened on the
/// affected major device, and tells each mounted filesystem about the
/// new driver endpoint via `req_newdriver()`.
///
/// C source: `minix/servers/vfs/device.c` — `bdev_up()` (line 681)
pub fn bdev_up(major: i32) {
    unsafe {
        let glob = vfs_global();
        let filp_arr = addr_of_mut!((*glob).filp) as *mut Filp;

        // Reopen block-special files on this major so the driver
        // connection is re-established.
        for i in 0..NR_FILPS {
            let f = &*filp_arr.add(i);
            if f.filp_mode == FILP_CLOSED {
                continue;
            }
            let vp = f.filp_vno;
            if vp.is_null() {
                continue;
            }
            if (*vp).v_mode & S_IFMT == S_IFBLK && ((*vp).v_dev >> 16) as i32 == major {
                // O_RDONLY = access mode 0 → read-only; otherwise r/w.
                let access = if (f.filp_flags & 0o3) != 0 {
                    BDEV_R_BIT | BDEV_W_BIT
                } else {
                    BDEV_R_BIT
                };
                let _ = bdev_open((*vp).v_dev, access);
            }
        }

        // Tell each mounted filesystem on this major about the new
        // driver endpoint (it re-resolves the driver label).
        let dp = dmap::get_dmap_by_major(major);
        if dp.is_null() {
            return;
        }
        let label_buf = &(*dp).dmap_label;
        let label_len = label_buf.iter().position(|&b| b == 0).unwrap_or(LABEL_MAX);
        let label = &label_buf[..label_len];

        let vmnt_arr = addr_of_mut!((*glob).vmnt) as *mut Vmnt;
        for i in 0..NR_MNTS {
            let vmp = &mut *vmnt_arr.add(i);
            if vmp.m_dev != u32::MAX && ((vmp.m_dev >> 16) as i32) == major {
                let _ = request::req_newdriver(vmp.m_fs_e, vmp.m_dev, label.as_ptr());
            }
        }
    }
}

/// Invalidate all character-special files on `major` (driver went away).
///
/// Marks every open char-special filp on the major as closed. Used by
/// `dmap_endpt_up` when a character driver restarts.
///
/// C source: `minix/servers/vfs/device.c` — `invalidate_filp_by_char_major()`
pub fn invalidate_filp_by_char_major(major: i32) {
    unsafe {
        let glob = vfs_global();
        let filp_arr = addr_of_mut!((*glob).filp) as *mut Filp;
        for i in 0..NR_FILPS {
            let f = &mut *filp_arr.add(i);
            if f.filp_mode == FILP_CLOSED {
                continue;
            }
            let vp = f.filp_vno;
            if vp.is_null() {
                continue;
            }
            if (*vp).v_mode & S_IFMT == S_IFCHR && ((*vp).v_dev >> 16) as i32 == major {
                f.filp_mode = FILP_CLOSED;
                f.filp_count = 0;
            }
        }
    }
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
