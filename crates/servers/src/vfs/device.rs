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

use arch_common::com::CDEV_MAP;
use arch_common::safecopies::{CPF_READ, CPF_WRITE, GRANT_INVALID};

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

/// Max bytes an ioctl arg struct travels in the CDEV_IOCTL data area.
///
/// Unused since the grant-based ioctl protocol: arg bytes now move through
/// a magic grant (`cdev_io`), so the 32-byte inline cap is gone. Kept to
/// document the old inline layout for the net driver's history.
#[allow(dead_code)]
const IOCTL_DATA_MAX: usize = 32;

/// Driver-level non-blocking flag (matches the tty server's CDEV_NONBLOCK).
const CDEV_NONBLOCK: i64 = 0x01;

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

    // CDEV_IOCTL: request code in m2_i2; the arg struct travels through a
    // magic grant (C device.c make_grant), so arbitrarily large arg structs
    // (e.g. termios, 44 bytes) work — the driver reads the caller's buffer
    // with sys_safecopyfrom and writes results back with sys_safecopyto.
    // The grant access follows the ioctl direction (_IOW → CPF_READ,
    // _IOR → CPF_WRITE); no-arg ioctls (size 0, e.g. TIOCSTART) pass
    // GRANT_INVALID and the driver leaves the buffer alone.
    //
    // CDEV_IOCTL message layout (matches the tty server's handle_cdev_request
    // reads): m2_i1 = minor, m2_i2 = request, m2_i3 = grant,
    // m2_l1 = user endpoint, m2_l2 = flags, m2_l3 = id.
    let request = bytes as u32;
    let arg_size = net::ioc_size(request);
    let mut grant_access = 0;
    if net::ioc_is_out(request) {
        grant_access |= CPF_WRITE;
    }
    if net::ioc_is_in(request) {
        grant_access |= CPF_READ;
    }
    let mut grant = GRANT_INVALID;
    if arg_size > 0 {
        grant =
            crate::vfs::grant::cpf_grant_magic_access(proc_e, drv_e, buf, arg_size, grant_access);
    }
    let nonblock_flag = if _flags & O_NONBLOCK != 0 {
        CDEV_NONBLOCK
    } else {
        0
    };
    let mut msg = build_cdev_ioctl_msg(minor, request, grant, proc_e, nonblock_flag);
    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if grant != GRANT_INVALID {
        crate::vfs::grant::cpf_revoke(grant);
    }
    // Deferred ioctl: the driver replied with CDEV_REPLY instead of a plain
    // status reply (C mess_lchardriver_vfs_reply: status @ m2_i1, id @ m2_i2).
    if r as u32 == arch_common::com::CDEV_REPLY {
        return request::r_i32(&msg, 8);
    }
    r
}

/// Build the grant-based CDEV_IOCTL request message. The arg struct travels
/// through the magic grant (`grant`, `GRANT_INVALID` for no-arg ioctls); the
/// user endpoint and flags travel in m2_l1/m2_l2 so the driver can identify
/// the real caller and honor non-blocking. Layout (matches the tty server's
/// handle_cdev_request): m2_i1 = minor, m2_i2 = request, m2_i3 = grant,
/// m2_l1 = user endpoint, m2_l2 = flags, m2_l3 = id.
fn build_cdev_ioctl_msg(minor: i32, request: u32, grant: i32, user: i32, flags: i64) -> [u8; 56] {
    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, CDEV_IOCTL);
    request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
    request::w_i32(&mut msg, CDEV_FLAGS_OFF, request as i32);
    request::w_i32(&mut msg, CDEV_USER_OFF, grant);
    request::w_i64(&mut msg, CDEV_POS_OFF, user as i64);
    request::w_i64(&mut msg, CDEV_COUNT_OFF, flags);
    request::w_i64(&mut msg, CDEV_BUF_OFF, 0); // id: the port's sync model needs none
    msg
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
/// Sends a `CDEV_SELECT` message to the driver via synchronous sendrec and
/// returns the currently-ready ops (`SEL_* == CDEV_OP_*`). `CDEV_NOTIFY`
/// asks the driver to register a late watch: if not everything is ready it
/// replies later with `CDEV_SEL2_REPLY` when the remaining ops become
/// ready (the tty's `select_retry`).
///
/// Wire layout (matches the tty's `handle_cdev_request`): m_type @ 4 =
/// `CDEV_SELECT`, m2_i1 @ 8 = minor, m2_i2 @ 12 = ops | CDEV_NOTIFY. The
/// reply type is the ready ops.
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
    request::w_i32(&mut msg, 4, CDEV_SELECT);
    request::w_i32(&mut msg, CDEV_MINOR_OFF, minor as i32);
    request::w_i32(
        &mut msg,
        CDEV_FLAGS_OFF,
        ops | arch_common::com::CDEV_NOTIFY as i32,
    );
    unsafe { request::fs_sendrec(drv_e, &mut msg) }
}

/// Ask a character driver for the physical range of its device memory
/// (mmap of a char device — the port's equivalent of the C dmap `map`
/// hook). Sends `CDEV_MAP`; the driver replies with the physical base
/// and length in the payload (u64 each).
///
/// Returns `(status, phys, len)`; on error phys/len are 0.
pub fn cdev_map_phys(dev: u32) -> (i32, u64, u64) {
    let dp = dmap::get_dmap_by_major((dev >> 16) as i32);
    if dp.is_null() {
        return (ENXIO, 0, 0);
    }
    let drv_e = unsafe { (*dp).dmap_ep };
    if drv_e < 0 {
        return (ENXIO, 0, 0);
    }
    let mut msg = build_cdev_map_msg((dev & 0xFFFF) as i32);
    let r = unsafe { request::fs_sendrec(drv_e, &mut msg) };
    if r < 0 {
        return (r, 0, 0);
    }
    parse_cdev_map_reply(&msg)
}

/// Build a CDEV_MAP request message (m_type @ 4, minor @ 8, flags @ 12).
fn build_cdev_map_msg(minor: i32) -> [u8; 56] {
    let mut msg = [0u8; 56];
    request::w_i32(&mut msg, 4, CDEV_MAP as i32);
    request::w_i32(&mut msg, CDEV_MINOR_OFF, minor);
    request::w_i32(&mut msg, CDEV_FLAGS_OFF, 0);
    msg
}

/// Parse a CDEV_MAP reply: status = m_type (returned by `fs_sendrec`),
/// phys u64 @ payload 0, len u64 @ payload 8. Pure so the layout is
/// host-testable.
pub fn parse_cdev_map_reply(msg: &[u8; 56]) -> (i32, u64, u64) {
    let status = i32::from_le_bytes(msg[4..8].try_into().unwrap_or([0; 4]));
    if status < 0 {
        return (status, 0, 0);
    }
    let phys = u64::from_le_bytes(msg[8..16].try_into().unwrap_or([0; 8]));
    let len = u64::from_le_bytes(msg[16..24].try_into().unwrap_or([0; 8]));
    (OK, phys, len)
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
/// * \`CDEV_REPLY\` — open/close/read/write/ioctl result (status @ payload 0,
///   id @ payload 4).
/// * \`CDEV_SEL1_REPLY\` / \`CDEV_SEL2_REPLY\` — select replies (minor @
///   payload 0, status @ payload 4).
///
/// In the port's synchronous model a \`CDEV_REPLY\` for a deferred ioctl is
/// consumed inline by the blocked sendrec in \`cdev_io\` (which extracts the
/// status there), so a reply reaching the main loop is a late/duplicate
/// reply or a select notification. VFS select is not implemented yet
/// (Phase I); the SEL arms are consumed and dropped here.
///
/// C source: \`minix/servers/vfs/device.c\` — \`cdev_reply()\` (line 794)
///
/// # Safety
///
/// Must be called from the VFS main loop when a \`CDEV_REPLY\`,
/// \`CDEV_SEL1_REPLY\`, or \`CDEV_SEL2_REPLY\` message is received.
pub fn cdev_reply() -> i32 {
    let glob = unsafe { &*vfs_global() };
    let m_in = &glob.fs_m_in;
    let call_nr = glob.req_nr as u32;
    match call_nr {
        arch_common::com::CDEV_SEL1_REPLY | arch_common::com::CDEV_SEL2_REPLY => {
            // Select notification: status (the ops now ready) @ payload 0
            // (m_in[8..12]), minor @ payload 4 (m_in[12..16]) — the layout
            // chardriver_reply_select builds. Route to the select machinery,
            // which sends the final reply to the blocked caller.
            let status = i32::from_le_bytes(m_in[8..12].try_into().unwrap_or([0; 4]));
            let minor = u32::from_le_bytes(m_in[12..16].try_into().unwrap_or([0; 4]));
            unsafe { crate::vfs::select::select_driver_reply(minor, status) }
        }
        _ => cdev_reply_from(m_in, call_nr),
    }
}

/// Parse a character-driver reply message and return the result status.
///
/// Pure over the message buffer so the layout is host-testable. All three
/// reply types carry the status first (C mess_lchardriver_vfs_reply /
/// _sel1 / _sel2: status @ payload 0):
///   CDEV_REPLY      → status @ m_in[8..12], id @ m_in[12..16]
///   CDEV_SEL1/2     → status @ m_in[8..12], minor @ m_in[12..16]
/// Anything else → EINVAL.
pub fn cdev_reply_from(m_in: &[u8; 64], call_nr: u32) -> i32 {
    match call_nr {
        arch_common::com::CDEV_REPLY
        | arch_common::com::CDEV_SEL1_REPLY
        | arch_common::com::CDEV_SEL2_REPLY => {
            i32::from_le_bytes(m_in[8..12].try_into().unwrap_or([0; 4]))
        }
        _ => EINVAL,
    }
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

// C's `bdev_reply()` (device.c line 824) processes asynchronous
// BDEV_REPLY completions from block drivers. This port's block path is
// sendrec-synchronous (bdev_open/bdev_close above reply inline via
// `fs_sendrec`), and the VFS main loop has no BDEV_REPLY branch, so that
// async handler is unreachable here and deliberately omitted.

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

/// TIOCGETA value used by the layout test (matches the tty server's real
/// `_IOR('t', 19, struct termios)` encoding; a bare constant keeps this test
/// module independent of the tty crate's constants).
#[cfg(test)]
const TIOCGETA_VALUE: u32 = 0x4000_0000 | ((44 & 0x1fff) << 16) | ((b't' as u32) << 8) | 19;

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
        let dev = 5u32 << 16; // major 5 (console), minor 0
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

    #[test]
    fn test_build_cdev_ioctl_msg_layout() {
        // Grant-based CDEV_IOCTL layout: m2_i1 = minor, m2_i2 = request,
        // m2_i3 = grant, m2_l1 = user endpoint, m2_l2 = flags, m2_l3 = id.
        // Must match the tty server's handle_cdev_request parse.
        let msg = build_cdev_ioctl_msg(3, TIOCGETA_VALUE, 99, 321, 1);
        assert_eq!(request::r_i32(&msg, 4), CDEV_IOCTL);
        assert_eq!(request::r_i32(&msg, CDEV_MINOR_OFF), 3);
        assert_eq!(request::r_i32(&msg, CDEV_FLAGS_OFF), TIOCGETA_VALUE as i32);
        assert_eq!(request::r_i32(&msg, CDEV_USER_OFF), 99);
        assert_eq!(request::r_i64(&msg, CDEV_POS_OFF), 321);
        assert_eq!(request::r_i64(&msg, CDEV_COUNT_OFF), 1);
        assert_eq!(request::r_i64(&msg, CDEV_BUF_OFF), 0);
    }

    #[test]
    fn test_build_cdev_map_msg_layout() {
        // CDEV_MAP: m_type @ 4, minor @ 8, flags @ 12 — the fb server's
        // handle_cdev_request reads m2_i1/m2_i2 at those payload offsets.
        let msg = build_cdev_map_msg(0);
        assert_eq!(request::r_i32(&msg, 4), CDEV_MAP as i32);
        assert_eq!(request::r_i32(&msg, CDEV_MINOR_OFF), 0);
        assert_eq!(request::r_i32(&msg, CDEV_FLAGS_OFF), 0);
    }

    #[test]
    fn test_parse_cdev_map_reply() {
        // The driver replies with status = m_type and phys/len in payload
        // 0..16 (absolute bytes 8..24).
        let mut msg = [0u8; 56];
        msg[4..8].copy_from_slice(&0i32.to_le_bytes());
        msg[8..16].copy_from_slice(&0xFD00_0000u64.to_le_bytes());
        msg[16..24].copy_from_slice(&0x100_0000u64.to_le_bytes());
        let (status, phys, len) = parse_cdev_map_reply(&msg);
        assert_eq!(status, 0);
        assert_eq!(phys, 0xFD00_0000);
        assert_eq!(len, 0x100_0000);

        // Error status → zeroed phys/len.
        msg[4..8].copy_from_slice(&(-6i32).to_le_bytes());
        let (status, phys, len) = parse_cdev_map_reply(&msg);
        assert_eq!(status, -6);
        assert_eq!(phys, 0);
        assert_eq!(len, 0);
    }

    #[test]
    fn test_cdev_reply_from_parses_status() {
        // C mess_lchardriver_vfs_reply: status @ payload 0, id @ payload 4;
        // the SEL1/SEL2 variants carry status at the same offset. A non-reply
        // call number is rejected.
        let mut m = [0u8; 64];
        m[4..8].copy_from_slice(&0u32.to_le_bytes());
        m[8..12].copy_from_slice(&7i32.to_le_bytes()); // status
        m[12..16].copy_from_slice(&42i32.to_le_bytes()); // id / minor
        assert_eq!(cdev_reply_from(&m, arch_common::com::CDEV_REPLY), 7);
        assert_eq!(cdev_reply_from(&m, arch_common::com::CDEV_SEL1_REPLY), 7);
        assert_eq!(cdev_reply_from(&m, arch_common::com::CDEV_SEL2_REPLY), 7);
        assert_eq!(cdev_reply_from(&m, 0x1234), EINVAL);
    }
}
