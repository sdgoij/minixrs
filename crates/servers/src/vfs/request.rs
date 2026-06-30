//! VFS request layer — FS communication wrappers.
//!
//! Each function in this module builds an FS request message, sends it to the
//! appropriate FS server (MFS, ext2, PFS, etc.) via IPC, and parses the
//! response.  Grant/copy operations are stubs until per-process grant tables
//! are wired (Phase 14+).
//!
//! Ported from `minix/servers/vfs/request.c`.

use crate::vfs::consts::ENOSYS;
#[cfg(target_os = "none")]
use crate::vfs::consts::PATH_MAX;
use crate::vfs::types::{Lookup, LookupRes, NodeDetails, Statvfs, off_t};

// FS_BASE and REQ_* constants (from minix/include/minix/vfsif.h)

#[allow(dead_code)]
const FS_BASE: i32 = 0xA00;

#[allow(dead_code)]
const REQ_PUTNODE: i32 = FS_BASE + 2;
#[allow(dead_code)]
const REQ_SLINK: i32 = FS_BASE + 3;
#[allow(dead_code)]
const REQ_FTRUNC: i32 = FS_BASE + 4;
#[allow(dead_code)]
const REQ_CHOWN: i32 = FS_BASE + 5;
#[allow(dead_code)]
const REQ_CHMOD: i32 = FS_BASE + 6;
#[allow(dead_code)]
const REQ_INHIBREAD: i32 = FS_BASE + 7;
#[allow(dead_code)]
const REQ_STAT: i32 = FS_BASE + 8;
#[allow(dead_code)]
const REQ_UTIME: i32 = FS_BASE + 9;
#[allow(dead_code)]
const REQ_STATVFS: i32 = FS_BASE + 10;
#[allow(dead_code)]
const REQ_BREAD: i32 = FS_BASE + 11;
#[allow(dead_code)]
const REQ_BWRITE: i32 = FS_BASE + 12;
#[allow(dead_code)]
const REQ_UNLINK: i32 = FS_BASE + 13;
#[allow(dead_code)]
const REQ_RMDIR: i32 = FS_BASE + 14;
#[allow(dead_code)]
const REQ_UNMOUNT: i32 = FS_BASE + 15;
#[allow(dead_code)]
const REQ_SYNC: i32 = FS_BASE + 16;
#[allow(dead_code)]
const REQ_NEW_DRIVER: i32 = FS_BASE + 17;
#[allow(dead_code)]
const REQ_FLUSH: i32 = FS_BASE + 18;
#[allow(dead_code)]
const REQ_READ: i32 = FS_BASE + 19;
#[allow(dead_code)]
const REQ_WRITE: i32 = FS_BASE + 20;
#[allow(dead_code)]
const REQ_MKNOD: i32 = FS_BASE + 21;
#[allow(dead_code)]
const REQ_MKDIR: i32 = FS_BASE + 22;
#[allow(dead_code)]
const REQ_CREATE: i32 = FS_BASE + 23;
#[allow(dead_code)]
const REQ_LINK: i32 = FS_BASE + 24;
#[allow(dead_code)]
const REQ_RENAME: i32 = FS_BASE + 25;
#[allow(dead_code)]
const REQ_LOOKUP: i32 = FS_BASE + 26;
#[allow(dead_code)]
const REQ_MOUNTPOINT: i32 = FS_BASE + 27;
#[allow(dead_code)]
const REQ_READSUPER: i32 = FS_BASE + 28;
#[allow(dead_code)]
const REQ_NEWNODE: i32 = FS_BASE + 29;
#[allow(dead_code)]
const REQ_RDLINK: i32 = FS_BASE + 30;
#[allow(dead_code)]
const REQ_GETDENTS: i32 = FS_BASE + 31;
#[allow(dead_code)]
const REQ_PEEK: i32 = FS_BASE + 32;
#[allow(dead_code)]
const REQ_BPEEK: i32 = FS_BASE + 33;

// VFS/FS flags (from vfsif.h)

#[allow(dead_code)]
const REQ_RDONLY: u32 = 0o01;
#[allow(dead_code)]
const REQ_ISROOT: u32 = 0o02;
#[allow(dead_code)]
const PATH_GET_UCRED: u32 = 0o20;

// FS capability flags (from vfsif.h)

#[allow(dead_code)]
const RES_64BIT: u32 = 0o04;

// Message buffer helpers

/// A 56-byte IPC message buffer (Message = m_source(4) + m_type(4) + payload(48)).
#[allow(dead_code)]
type MsgBuf = [u8; 56];

/// Offset of the m_type field within MsgBuf.
#[allow(dead_code)]
const M_TYPE_OFF: usize = 4;

/// Payload (union) starts at byte 8.
#[allow(dead_code)]
const PAYLOAD_OFF: usize = 8;

// Little-endian write helpers

#[allow(dead_code)]
#[inline]
pub(crate) fn w_i32(buf: &mut MsgBuf, off: usize, val: i32) {
    buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
}
#[allow(dead_code)]
#[inline]
pub(crate) fn w_u32(buf: &mut MsgBuf, off: usize, val: u32) {
    buf[off..off + 4].copy_from_slice(&val.to_le_bytes());
}
#[allow(dead_code)]
#[inline]
fn w_u16(buf: &mut MsgBuf, off: usize, val: u16) {
    buf[off..off + 2].copy_from_slice(&val.to_le_bytes());
}
#[allow(dead_code)]
#[inline]
pub(crate) fn w_i64(buf: &mut MsgBuf, off: usize, val: i64) {
    buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
}
#[allow(dead_code)]
#[inline]
pub(crate) fn w_u64(buf: &mut MsgBuf, off: usize, val: u64) {
    buf[off..off + 8].copy_from_slice(&val.to_le_bytes());
}

// Little-endian read helpers

#[allow(dead_code)]
#[inline]
pub(crate) fn r_i32(buf: &MsgBuf, off: usize) -> i32 {
    i32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
#[allow(dead_code)]
#[inline]
pub(crate) fn r_u32(buf: &MsgBuf, off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
#[allow(dead_code)]
#[inline]
fn r_u16(buf: &MsgBuf, off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}
#[allow(dead_code)]
#[inline]
pub(crate) fn r_i64(buf: &MsgBuf, off: usize) -> i64 {
    i64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}
#[allow(dead_code)]
#[inline]
fn r_u64(buf: &MsgBuf, off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

// IPC send/recv

/// Low-level IPC send/recv with a FS server.
///
/// Sends `msg` (with `m_type` already set by the caller) to `fs_e`, receives
/// the reply in the same buffer.  Returns OK (0) on success or a negative
/// errno.
///
/// # Safety
///
/// `msg` must point to a valid, mutable 56-byte message buffer.
pub unsafe fn fs_sendrec(fs_e: i32, msg: &mut MsgBuf) -> i32 {
    #[cfg(target_os = "none")]
    {
        let m = &mut *(msg.as_mut_ptr() as *mut arch_common::ipc::Message);
        m.m_type = r_i32(msg, M_TYPE_OFF);
        match minix_std::sendrec(fs_e, m) {
            Ok(src) => {
                // Copy m_source back (kernel sets it on receive)
                w_i32(msg, 0, src);
                crate::vfs::consts::OK
            }
            Err(e) => e.0,
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, msg);
        -ENOSYS
    }
}

// Block-oriented operations

/// Block read/write with grant-based buffers and EFAULT retry via VM handlemem.
///
/// Returns `(status, new_pos, cum_iop)`.
///
/// # Safety
///
/// `user_addr` must be a valid user-space address or null; caller must ensure
/// endpoint validity.
#[allow(clippy::too_many_arguments)]
pub unsafe fn req_breadwrite(
    fs_e: i32,
    _user_e: i32,
    dev: u32,
    pos: off_t,
    num_of_bytes: u32,
    _user_addr: *const u8,
    rw_flag: i32,
) -> (i32, off_t, u32) {
    #[cfg(target_os = "none")]
    {
        // Grant stubbed — see 13.10b: wire grant IDs for data transfer
        let _grant_id: i32 = -1;

        let mut msg = [0u8; 56];
        w_i32(
            &mut msg,
            M_TYPE_OFF,
            if rw_flag == 1 /* READING */ {
                REQ_BREAD
            } else {
                REQ_BWRITE
            },
        );
        w_u32(&mut msg, PAYLOAD_OFF, dev); // device
        w_i64(&mut msg, PAYLOAD_OFF + 8, pos); // seek_pos
        w_i32(&mut msg, PAYLOAD_OFF + 16, -1); // grant (stub)
        w_u64(&mut msg, PAYLOAD_OFF + 24, num_of_bytes as u64); // nbytes

        let r = fs_sendrec(fs_e, &mut msg);
        // cpf_revoke(grant_id) — stubbed

        if r != crate::vfs::consts::OK {
            return (r, 0, 0);
        }

        let new_pos = r_i64(&msg, PAYLOAD_OFF); // seek_pos (reply)
        let cum_iop = r_u64(&msg, PAYLOAD_OFF + 8) as u32; // nbytes (reply)
        (r, new_pos, cum_iop)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, _user_e, dev, pos, num_of_bytes, _user_addr, rw_flag);
        (ENOSYS, 0, 0)
    }
}

/// Block peek — query readable bytes at a device position without consuming.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_bpeek(fs_e: i32, dev: u32, pos: off_t, num_of_bytes: u32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_BPEEK);
        w_u32(&mut msg, PAYLOAD_OFF, dev); // device
        w_i64(&mut msg, PAYLOAD_OFF + 8, pos); // seek_pos
        w_u64(&mut msg, PAYLOAD_OFF + 24, num_of_bytes as u64); // nbytes

        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, dev, pos, num_of_bytes);
        ENOSYS
    }
}

// Inode metadata operations

/// Change mode of an inode.
///
/// Returns `(status, new_mode)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_chmod(fs_e: i32, inode_nr: u32, rmode: u32) -> (i32, u32) {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_CHMOD);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_u16(&mut msg, PAYLOAD_OFF + 4, rmode as u16); // mode

        let r = fs_sendrec(fs_e, &mut msg);
        let new_mode = r_u16(&msg, PAYLOAD_OFF) as u32; // mode (reply)
        (r, new_mode)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, rmode);
        (ENOSYS, 0)
    }
}

/// Change owner of an inode.
///
/// Returns `(status, new_mode)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_chown(fs_e: i32, inode_nr: u32, newuid: u16, newgid: u16) -> (i32, u32) {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_CHOWN);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_u16(&mut msg, PAYLOAD_OFF + 4, newuid); // uid
        w_u16(&mut msg, PAYLOAD_OFF + 6, newgid); // gid

        let r = fs_sendrec(fs_e, &mut msg);
        let new_mode = r_u16(&msg, PAYLOAD_OFF) as u32; // mode (reply)
        (r, new_mode)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, newuid, newgid);
        (ENOSYS, 0)
    }
}

// File creation / destruction

/// Create a file.
///
/// Returns `(status, node_details)`.
///
/// # Safety
///
/// `path` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_create(
    fs_e: i32,
    inode_nr: u32,
    omode: i32,
    uid: u16,
    gid: u16,
    _path: *const u8,
) -> (i32, NodeDetails) {
    #[cfg(target_os = "none")]
    {
        let path_len = if _path.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_path).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _path as u64,
            path_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_CREATE);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_u16(&mut msg, PAYLOAD_OFF + 4, omode as u16); // mode
        w_u16(&mut msg, PAYLOAD_OFF + 6, uid); // uid
        w_u16(&mut msg, PAYLOAD_OFF + 8, gid); // gid
        w_i32(&mut msg, PAYLOAD_OFF + 12, grant_id);
        w_u64(&mut msg, PAYLOAD_OFF + 20, path_len as u64);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);

        if r != crate::vfs::consts::OK {
            return (r, NodeDetails::default());
        }

        (
            r,
            NodeDetails {
                inode_nr: r_u32(&msg, PAYLOAD_OFF + 8),     // inode (reply)
                mode: r_u16(&msg, PAYLOAD_OFF + 12) as u32, // mode (reply)
                file_size: r_i64(&msg, PAYLOAD_OFF),        // file_size (reply)
                dev: 0xffff,                                // NO_DEV
            },
        )
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, omode, uid, gid, _path);
        (ENOSYS, NodeDetails::default())
    }
}

/// Flush a device.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_flush(fs_e: i32, dev: u32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_FLUSH);
        w_u32(&mut msg, PAYLOAD_OFF, dev); // device
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, dev);
        ENOSYS
    }
}

/// Get filesystem statistics (statvfs).
///
/// Returns `(status, statvfs)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_statvfs(fs_e: i32) -> (i32, Statvfs) {
    #[cfg(target_os = "none")]
    {
        // Grant stubbed — FS writes statvfs data via grant, so the returned
        // Statvfs will be default-valued until grant table is wired.
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_STATVFS);
        w_i32(&mut msg, PAYLOAD_OFF, -1); // grant (stub)

        let r = fs_sendrec(fs_e, &mut msg);
        // cpf_revoke(grant_id) — stubbed

        (r, Statvfs::default())
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = fs_e;
        (ENOSYS, Statvfs::default())
    }
}

/// Truncate a file between `start` and `end`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_ftrunc(fs_e: i32, inode_nr: u32, start: off_t, end: off_t) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_FTRUNC);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i64(&mut msg, PAYLOAD_OFF + 8, start); // trc_start
        w_i64(&mut msg, PAYLOAD_OFF + 16, end); // trc_end
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, start, end);
        ENOSYS
    }
}

/// Read directory entries.
///
/// Returns `(status, new_pos)`.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `size` bytes.  Caller must
/// ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_getdents(
    fs_e: i32,
    inode_nr: u32,
    pos: off_t,
    _buf: *mut u8,
    size: usize,
    _direct: i32,
) -> (i32, off_t) {
    #[cfg(target_os = "none")]
    {
        // Grant stubbed — TODO: wire grant table
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_GETDENTS);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i64(&mut msg, PAYLOAD_OFF + 8, pos); // seek_pos
        w_i32(&mut msg, PAYLOAD_OFF + 16, -1); // grant (stub)
        w_u64(&mut msg, PAYLOAD_OFF + 24, size as u64); // mem_size

        let r = fs_sendrec(fs_e, &mut msg);
        // cpf_revoke(grant_id) — stubbed

        if r != crate::vfs::consts::OK {
            return (r, 0);
        }

        let new_pos = r_i64(&msg, PAYLOAD_OFF); // seek_pos (reply)
        (r, new_pos)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, pos, _buf, size, _direct);
        (ENOSYS, 0)
    }
}

/// Inhibit read — tell the FS not to issue read-ahead.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_inhibread(fs_e: i32, inode_nr: u32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_INHIBREAD);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr);
        ENOSYS
    }
}

// Link / unlink

/// Create a hard link.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_link(fs_e: i32, link_parent: u32, _lastc: *const u8, linked_file: u32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let path_len = if _lastc.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_lastc).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _lastc as u64,
            path_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_LINK);
        w_u32(&mut msg, PAYLOAD_OFF, linked_file); // inode
        w_u32(&mut msg, PAYLOAD_OFF + 4, link_parent); // dir_ino
        w_i32(&mut msg, PAYLOAD_OFF + 8, grant_id);
        w_u64(&mut msg, PAYLOAD_OFF + 16, path_len as u64);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, link_parent, _lastc, linked_file);
        ENOSYS
    }
}

// Path lookup

/// Path lookup — resolve a path relative to `dir_ino`.
///
/// Returns `(status, lookup_result)`.
///
/// # Safety
///
/// `resolve` must point to a valid `lookup` structure.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_lookup(
    fs_e: i32,
    dir_ino: u32,
    root_ino: u32,
    uid: u16,
    gid: u16,
    resolve: &Lookup,
) -> (i32, LookupRes) {
    #[cfg(target_os = "none")]
    {
        // Grants stubbed — TODO: wire grant table
        let flags: u32 = resolve.l_flags;

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_LOOKUP);
        w_u32(&mut msg, PAYLOAD_OFF, dir_ino); // dir_ino
        w_u32(&mut msg, PAYLOAD_OFF + 4, root_ino); // root_ino
        w_u32(&mut msg, PAYLOAD_OFF + 8, flags); // flags

        // Copy path to message payload.
        #[cfg(target_os = "none")]
        let path_len = resolve.l_path_len.min(PATH_MAX - 1);
        #[cfg(not(target_os = "none"))]
        let path_len = resolve.l_path_len;
        w_u32(&mut msg, PAYLOAD_OFF + 12, path_len as u32);
        if path_len > 0 {
            let msg_path = &mut msg[PAYLOAD_OFF + 16..PAYLOAD_OFF + 16 + path_len];
            let src_path = &resolve.l_path[..path_len];
            let copy_len = msg_path.len().min(src_path.len());
            msg_path[..copy_len].copy_from_slice(&src_path[..copy_len]);
        }

        w_u32(&mut msg, PAYLOAD_OFF + 32, 0); // ucred_size
        w_i32(&mut msg, PAYLOAD_OFF + 40, -1); // grant_path (stub)
        w_i32(&mut msg, PAYLOAD_OFF + 44, -1); // grant_ucred (stub)
        w_u16(&mut msg, PAYLOAD_OFF + 48, uid); // uid
        w_u16(&mut msg, PAYLOAD_OFF + 50, gid); // gid

        let r = fs_sendrec(fs_e, &mut msg);
        // cpf_revoke(grant_id); cpf_revoke(grant_id2) — stubbed

        let mut res = LookupRes::default();
        res.fs_e = r_i32(&msg, 0); // m_source

        if r == crate::vfs::consts::OK {
            res.inode_nr = r_u32(&msg, PAYLOAD_OFF + 20); // inode
            res.mode = r_u32(&msg, PAYLOAD_OFF + 24); // mode
            res.file_size = r_i64(&msg, PAYLOAD_OFF + 8); // file_size
            res.dev = r_u32(&msg, PAYLOAD_OFF + 16); // device
        }

        (r, res)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, dir_ino, root_ino, uid, gid, resolve);
        (ENOSYS, LookupRes::default())
    }
}

// Directory operations

/// Create a directory.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_mkdir(
    fs_e: i32,
    inode_nr: u32,
    _lastc: *const u8,
    uid: u16,
    gid: u16,
    dmode: u32,
) -> i32 {
    #[cfg(target_os = "none")]
    {
        let path_len = if _lastc.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_lastc).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _lastc as u64,
            path_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_MKDIR);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_u16(&mut msg, PAYLOAD_OFF + 4, dmode as u16); // mode
        w_u16(&mut msg, PAYLOAD_OFF + 6, uid); // uid
        w_u16(&mut msg, PAYLOAD_OFF + 8, gid); // gid
        w_i32(&mut msg, PAYLOAD_OFF + 12, grant_id);
        w_u64(&mut msg, PAYLOAD_OFF + 20, path_len as u64);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _lastc, uid, gid, dmode);
        ENOSYS
    }
}

/// Create a special file (device node).
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
#[allow(clippy::too_many_arguments)]
pub unsafe fn req_mknod(
    fs_e: i32,
    inode_nr: u32,
    _lastc: *const u8,
    uid: u16,
    gid: u16,
    dmode: u32,
    dev: u32,
) -> i32 {
    #[cfg(target_os = "none")]
    {
        let path_len = if _lastc.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_lastc).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _lastc as u64,
            path_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_MKNOD);
        w_u32(&mut msg, PAYLOAD_OFF, dev); // device
        w_u32(&mut msg, PAYLOAD_OFF + 4, inode_nr); // inode
        w_u16(&mut msg, PAYLOAD_OFF + 8, dmode as u16); // mode
        w_u16(&mut msg, PAYLOAD_OFF + 10, uid); // uid
        w_u16(&mut msg, PAYLOAD_OFF + 12, gid); // gid
        w_i32(&mut msg, PAYLOAD_OFF + 16, grant_id);
        w_u64(&mut msg, PAYLOAD_OFF + 24, path_len as u64);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _lastc, uid, gid, dmode, dev);
        ENOSYS
    }
}

/// Check whether an inode is a mount point.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_mountpoint(fs_e: i32, inode_nr: u32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_MOUNTPOINT);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr);
        ENOSYS
    }
}

/// Create a new inode.
///
/// Returns `(status, node_details)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_newnode(
    fs_e: i32,
    uid: u16,
    gid: u16,
    dmode: u32,
    dev: u32,
) -> (i32, NodeDetails) {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_NEWNODE);
        w_u32(&mut msg, PAYLOAD_OFF, dev); // device
        w_u16(&mut msg, PAYLOAD_OFF + 4, dmode as u16); // mode
        w_u16(&mut msg, PAYLOAD_OFF + 6, uid); // uid
        w_u16(&mut msg, PAYLOAD_OFF + 8, gid); // gid

        let r = fs_sendrec(fs_e, &mut msg);

        if r != crate::vfs::consts::OK {
            return (r, NodeDetails::default());
        }

        (
            r,
            NodeDetails {
                inode_nr: r_u32(&msg, PAYLOAD_OFF + 12),    // inode (reply)
                mode: r_u16(&msg, PAYLOAD_OFF + 16) as u32, // mode (reply)
                file_size: r_i64(&msg, PAYLOAD_OFF),        // file_size (reply)
                dev: r_u32(&msg, PAYLOAD_OFF + 8),          // device (reply)
            },
        )
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, uid, gid, dmode, dev);
        (ENOSYS, NodeDetails::default())
    }
}

/// Notify a FS about a new driver.
///
/// # Safety
///
/// `label` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_newdriver(fs_e: i32, dev: u32, _label: *const u8) -> i32 {
    #[cfg(target_os = "none")]
    {
        let path_len = if _label.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_label).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _label as u64,
            path_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_NEW_DRIVER);
        w_u32(&mut msg, PAYLOAD_OFF, dev); // device
        w_i32(&mut msg, PAYLOAD_OFF + 8, grant_id);
        w_u64(&mut msg, PAYLOAD_OFF + 16, path_len as u64);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, dev, _label);
        ENOSYS
    }
}

// Inode ref-counting

/// Release an inode reference.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_putnode(fs_e: i32, inode_nr: u32, count: i32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_PUTNODE);
        w_u64(&mut msg, PAYLOAD_OFF, count as u64); // count
        w_u32(&mut msg, PAYLOAD_OFF + 8, inode_nr); // inode
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, count);
        ENOSYS
    }
}

// Readlink

/// Read the target of a symbolic link.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `len` bytes.  Caller must
/// ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_rdlink(
    fs_e: i32,
    inode_nr: u32,
    _proc_e: i32,
    _buf: *mut u8,
    len: usize,
    _direct: i32,
) -> i32 {
    #[cfg(target_os = "none")]
    {
        // Grant stubbed — TODO: wire grant table
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_RDLINK);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i32(&mut msg, PAYLOAD_OFF + 8, -1); // grant (stub)
        w_u64(&mut msg, PAYLOAD_OFF + 16, len as u64); // mem_size

        let r = fs_sendrec(fs_e, &mut msg);
        // cpf_revoke(grant_id) — stubbed

        if r == crate::vfs::consts::OK {
            r_u64(&msg, PAYLOAD_OFF) as i32 // nbytes (reply)
        } else {
            r
        }
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _proc_e, _buf, len, _direct);
        ENOSYS
    }
}

// Superblock

/// Read the superblock of a filesystem.
///
/// Returns `(status, node_details, fs_flags)`.
///
/// # Safety
///
/// `vmp` must point to a valid `Vmnt` structure.  `label` must point to a
/// valid NUL-terminated string if non-null.  Caller must ensure endpoint
/// validity.
pub unsafe fn req_readsuper(
    _fs_e: i32,
    _label: *const u8,
    dev: u32,
    readonly: i32,
    isroot: i32,
) -> (i32, NodeDetails, u32) {
    #[cfg(target_os = "none")]
    {
        let label_len = if _label.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_label).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            _fs_e,
            _label as u64,
            label_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_READSUPER);
        let mut flags: u32 = 0;
        if readonly != 0 {
            flags |= REQ_RDONLY;
        }
        if isroot != 0 {
            flags |= REQ_ISROOT;
        }
        w_u32(&mut msg, PAYLOAD_OFF + 4, flags); // flags
        w_i32(&mut msg, PAYLOAD_OFF + 24, grant_id);
        w_u32(&mut msg, PAYLOAD_OFF, dev); // device
        w_u64(&mut msg, PAYLOAD_OFF + 8, label_len as u64);

        let r = fs_sendrec(_fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);

        if r != crate::vfs::consts::OK {
            return (r, NodeDetails::default(), 0);
        }

        (
            r,
            NodeDetails {
                inode_nr: r_u32(&msg, PAYLOAD_OFF + 12),    // inode (reply)
                mode: r_u16(&msg, PAYLOAD_OFF + 20) as u32, // mode (reply)
                file_size: r_i64(&msg, PAYLOAD_OFF),        // file_size (reply)
                dev: r_u32(&msg, PAYLOAD_OFF + 8),          // device (reply)
            },
            r_u32(&msg, PAYLOAD_OFF + 16), // flags (reply)
        )
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (_label, dev, readonly, isroot);
        (ENOSYS, NodeDetails::default(), 0)
    }
}

// Rename

/// Rename a file or directory.
///
/// # Safety
///
/// `old_name` and `new_name` must point to valid NUL-terminated strings.
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_rename(
    fs_e: i32,
    old_parent: u32,
    _old_name: *const u8,
    new_parent: u32,
    _new_name: *const u8,
) -> i32 {
    #[cfg(target_os = "none")]
    {
        let len_old = if _old_name.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_old_name).to_bytes().len() + 1
        };
        let len_new = if _new_name.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_new_name).to_bytes().len() + 1
        };
        let gid_old = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _old_name as u64,
            len_old,
        );
        let gid_new = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _new_name as u64,
            len_new,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_RENAME);
        w_u32(&mut msg, PAYLOAD_OFF, old_parent); // dir_old
        w_u32(&mut msg, PAYLOAD_OFF + 4, new_parent); // dir_new
        w_u64(&mut msg, PAYLOAD_OFF + 8, len_old as u64);
        w_u64(&mut msg, PAYLOAD_OFF + 16, len_new as u64);
        w_i32(&mut msg, PAYLOAD_OFF + 24, gid_old);
        w_i32(&mut msg, PAYLOAD_OFF + 28, gid_new);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(gid_old);
        crate::vfs::grant::cpf_revoke(gid_new);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, old_parent, _old_name, new_parent, _new_name);
        ENOSYS
    }
}

/// Remove a directory.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_rmdir(fs_e: i32, inode_nr: u32, _lastc: *const u8) -> i32 {
    #[cfg(target_os = "none")]
    {
        let path_len = if _lastc.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_lastc).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _lastc as u64,
            path_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_RMDIR);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i32(&mut msg, PAYLOAD_OFF + 8, grant_id);
        w_u64(&mut msg, PAYLOAD_OFF + 16, path_len as u64);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _lastc);
        ENOSYS
    }
}

// Symlink

/// Create a symbolic link.
///
/// # Safety
///
/// `lastc` and `path` must point to valid NUL-terminated strings.  Caller
/// must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_slink(
    fs_e: i32,
    inode_nr: u32,
    _lastc: *const u8,
    uid: u16,
    gid: u16,
    _path: *const u8,
) -> i32 {
    #[cfg(target_os = "none")]
    {
        let len_name = if _lastc.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_lastc).to_bytes().len() + 1
        };
        let len_buf = if _path.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_path).to_bytes().len() + 1
        };
        let gid_name = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _lastc as u64,
            len_name,
        );
        let gid_buf = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _path as u64,
            len_buf,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_SLINK);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_u64(&mut msg, PAYLOAD_OFF + 8, len_name as u64);
        w_u64(&mut msg, PAYLOAD_OFF + 16, len_buf as u64);
        w_i32(&mut msg, PAYLOAD_OFF + 24, gid_name);
        w_i32(&mut msg, PAYLOAD_OFF + 28, gid_buf);
        w_u16(&mut msg, PAYLOAD_OFF + 32, uid); // uid
        w_u16(&mut msg, PAYLOAD_OFF + 34, gid); // gid

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(gid_name);
        crate::vfs::grant::cpf_revoke(gid_buf);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _lastc, uid, gid, _path);
        ENOSYS
    }
}

// Stat

/// Stat an inode — write `struct stat` into a user-provided buffer.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `len` bytes.  Caller must
/// ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_stat(fs_e: i32, inode_nr: u32, _who_e: i32, _buf: *mut u8, _len: usize) -> i32 {
    #[cfg(target_os = "none")]
    {
        // Grant stubbed — TODO: wire grant table
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_STAT);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i32(&mut msg, PAYLOAD_OFF + 8, -1); // grant (stub)

        let r = fs_sendrec(fs_e, &mut msg);
        // cpf_revoke(grant_id) — stubbed
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _who_e, _buf, _len);
        ENOSYS
    }
}

// Sync

/// Sync a filesystem.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_sync(fs_e: i32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_SYNC);
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = fs_e;
        ENOSYS
    }
}

// Unlink / unmount

/// Remove a (non-directory) name from a directory.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_unlink(fs_e: i32, inode_nr: u32, _lastc: *const u8) -> i32 {
    #[cfg(target_os = "none")]
    {
        let path_len = if _lastc.is_null() {
            0
        } else {
            core::ffi::CStr::from_ptr(_lastc).to_bytes().len() + 1
        };
        let grant_id = crate::vfs::grant::cpf_grant_magic(
            arch_common::com::VFS_PROC_NR,
            fs_e,
            _lastc as u64,
            path_len,
        );

        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_UNLINK);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i32(&mut msg, PAYLOAD_OFF + 8, grant_id);
        w_u64(&mut msg, PAYLOAD_OFF + 16, path_len as u64);

        let r = fs_sendrec(fs_e, &mut msg);
        crate::vfs::grant::cpf_revoke(grant_id);
        r
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _lastc);
        ENOSYS
    }
}

/// Unmount a filesystem.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_unmount(fs_e: i32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_UNMOUNT);
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = fs_e;
        ENOSYS
    }
}

// Timestamps

/// Set access and modification times of an inode.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_utime(fs_e: i32, inode_nr: u32, actime: off_t, modtime: off_t) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_UTIME);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i64(&mut msg, PAYLOAD_OFF + 8, actime); // actime
        w_i64(&mut msg, PAYLOAD_OFF + 16, modtime); // modtime
        // acnsec and modnsec default to 0 (buffer is zeroed)
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, actime, modtime);
        ENOSYS
    }
}

// Read / Write (regular file oriented)

/// Peek at a regular file — read data at position without advancing state.
///
/// Used by the VM page fault handler (VMVFSREQ_FDIO) to page in file-backed
/// mmap regions. Does not update the file position on the FS server.
///
/// Returns the status code (0 = OK, negative = errno).
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_peek(fs_e: i32, inode_nr: u32, pos: off_t, bytes: u32) -> i32 {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_PEEK);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr);
        w_i64(&mut msg, PAYLOAD_OFF + 8, pos);
        w_i32(&mut msg, PAYLOAD_OFF + 16, -1); // grant (stub)
        w_u64(&mut msg, PAYLOAD_OFF + 24, bytes as u64);
        fs_sendrec(fs_e, &mut msg)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, pos, bytes);
        ENOSYS
    }
}

/// Read from a regular file.
///
/// Returns `(status, new_pos)`.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `size` bytes.  Caller must
/// ensure `fs_e` and `user_e` are valid endpoints.
pub unsafe fn req_read(
    fs_e: i32,
    inode_nr: u32,
    _buf: *mut u8,
    pos: off_t,
    size: u32,
    _user_e: i32,
    _direct: i32,
) -> (i32, off_t) {
    #[cfg(target_os = "none")]
    {
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_READ);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr);
        w_i64(&mut msg, PAYLOAD_OFF + 8, pos);
        w_i32(&mut msg, PAYLOAD_OFF + 16, -1);
        w_u64(&mut msg, PAYLOAD_OFF + 24, size as u64);

        let r = fs_sendrec(fs_e, &mut msg);

        if r != crate::vfs::consts::OK {
            return (r, 0);
        }

        let new_pos = r_i64(&msg, PAYLOAD_OFF);
        (r, new_pos)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _buf, pos, size, _user_e, _direct);
        (ENOSYS, 0)
    }
}

/// Write to a regular file.
///
/// Returns `(status, new_pos)`.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `size` bytes.  Caller must
/// ensure `fs_e` and `user_e` are valid endpoints.
pub unsafe fn req_write(
    fs_e: i32,
    inode_nr: u32,
    _buf: *const u8,
    pos: off_t,
    size: u32,
    _user_e: i32,
    _direct: i32,
) -> (i32, off_t) {
    #[cfg(target_os = "none")]
    {
        // Grant stubbed — TODO: wire grant table
        let mut msg = [0u8; 56];
        w_i32(&mut msg, M_TYPE_OFF, REQ_WRITE);
        w_u32(&mut msg, PAYLOAD_OFF, inode_nr); // inode
        w_i64(&mut msg, PAYLOAD_OFF + 8, pos); // seek_pos
        w_i32(&mut msg, PAYLOAD_OFF + 16, -1); // grant (stub)
        w_u64(&mut msg, PAYLOAD_OFF + 24, size as u64); // nbytes

        let r = fs_sendrec(fs_e, &mut msg);
        // cpf_revoke(grant_id) — stubbed

        if r != crate::vfs::consts::OK {
            return (r, 0);
        }

        let new_pos = r_i64(&msg, PAYLOAD_OFF); // seek_pos (reply)
        (r, new_pos)
    }
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fs_e, inode_nr, _buf, pos, size, _user_e, _direct);
        (ENOSYS, 0)
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_req_constants() {
        assert_eq!(FS_BASE, 0xA00);
        assert_eq!(REQ_PUTNODE, 0xA02);
        assert_eq!(REQ_SLINK, 0xA03);
        assert_eq!(REQ_FTRUNC, 0xA04);
        assert_eq!(REQ_CHOWN, 0xA05);
        assert_eq!(REQ_CHMOD, 0xA06);
        assert_eq!(REQ_INHIBREAD, 0xA07);
        assert_eq!(REQ_STAT, 0xA08);
        assert_eq!(REQ_UTIME, 0xA09);
        assert_eq!(REQ_STATVFS, 0xA0A);
        assert_eq!(REQ_BREAD, 0xA0B);
        assert_eq!(REQ_BWRITE, 0xA0C);
        assert_eq!(REQ_UNLINK, 0xA0D);
        assert_eq!(REQ_RMDIR, 0xA0E);
        assert_eq!(REQ_UNMOUNT, 0xA0F);
        assert_eq!(REQ_SYNC, 0xA10);
        assert_eq!(REQ_NEW_DRIVER, 0xA11);
        assert_eq!(REQ_FLUSH, 0xA12);
        assert_eq!(REQ_READ, 0xA13);
        assert_eq!(REQ_WRITE, 0xA14);
        assert_eq!(REQ_MKNOD, 0xA15);
        assert_eq!(REQ_MKDIR, 0xA16);
        assert_eq!(REQ_CREATE, 0xA17);
        assert_eq!(REQ_LINK, 0xA18);
        assert_eq!(REQ_RENAME, 0xA19);
        assert_eq!(REQ_LOOKUP, 0xA1A);
        assert_eq!(REQ_MOUNTPOINT, 0xA1B);
        assert_eq!(REQ_READSUPER, 0xA1C);
        assert_eq!(REQ_NEWNODE, 0xA1D);
        assert_eq!(REQ_RDLINK, 0xA1E);
        assert_eq!(REQ_GETDENTS, 0xA1F);
        assert_eq!(REQ_PEEK, 0xA20);
        assert_eq!(REQ_BPEEK, 0xA21);
    }

    #[test]
    fn test_fs_sendrec_returns_enosys_on_host() {
        let mut msg = [0u8; 56];
        let r = unsafe { fs_sendrec(0, &mut msg) };
        assert_eq!(r, -ENOSYS);
    }

    #[test]
    fn test_no_grant_helpers_return_enosys_on_host() {
        let r = unsafe { req_bpeek(0, 0, 0, 0) };
        assert_eq!(r, ENOSYS);

        let (s, _mode) = unsafe { req_chmod(0, 0, 0) };
        assert_eq!(s, ENOSYS);

        let (s, _mode) = unsafe { req_chown(0, 0, 0, 0) };
        assert_eq!(s, ENOSYS);

        let r = unsafe { req_flush(0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_ftrunc(0, 0, 0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_inhibread(0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_mountpoint(0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_sync(0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_unmount(0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_utime(0, 0, 0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_putnode(0, 0, 0) };
        assert_eq!(r, ENOSYS);
    }

    #[test]
    fn test_grant_stub_functions_return_enosys_on_host() {
        let r = unsafe { req_link(0, 0, core::ptr::null(), 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_mkdir(0, 0, core::ptr::null(), 0, 0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_mknod(0, 0, core::ptr::null(), 0, 0, 0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_newdriver(0, 0, core::ptr::null()) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_rdlink(0, 0, 0, core::ptr::null_mut(), 0, 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_rmdir(0, 0, core::ptr::null()) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_slink(0, 0, core::ptr::null(), 0, 0, core::ptr::null()) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_stat(0, 0, 0, core::ptr::null_mut(), 0) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_unlink(0, 0, core::ptr::null()) };
        assert_eq!(r, ENOSYS);

        let r = unsafe { req_rename(0, 0, core::ptr::null(), 0, core::ptr::null()) };
        assert_eq!(r, ENOSYS);
    }

    #[test]
    fn test_tuple_returns_are_enosys_on_host() {
        let (s, _det) = unsafe { req_create(0, 0, 0, 0, 0, core::ptr::null()) };
        assert_eq!(s, ENOSYS);

        let (s, _det) = unsafe { req_newnode(0, 0, 0, 0, 0) };
        assert_eq!(s, ENOSYS);

        let (s, _det, _fl) = unsafe { req_readsuper(0, core::ptr::null(), 0, 0, 0) };
        assert_eq!(s, ENOSYS);

        let (s, _st) = unsafe { req_statvfs(0) };
        assert_eq!(s, ENOSYS);

        let (s, _pos) = unsafe { req_getdents(0, 0, 0, core::ptr::null_mut(), 0, 0) };
        assert_eq!(s, ENOSYS);

        let (s, _pos) = unsafe { req_write(0, 0, core::ptr::null(), 0, 0, 0, 0) };
        assert_eq!(s, ENOSYS);

        let (s, _pos) = unsafe { req_read(0, 0, core::ptr::null_mut(), 0, 0, 0, 0) };
        assert_eq!(s, ENOSYS);

        let (s, _pos, _iop) = unsafe { req_breadwrite(0, 0, 0, 0, 0, core::ptr::null(), 0) };
        assert_eq!(s, ENOSYS);

        let (s, _res) = unsafe {
            let lookup = Lookup::default();
            req_lookup(0, 0, 0, 0, 0, &lookup)
        };
        assert_eq!(s, ENOSYS);
    }

    #[test]
    fn test_msg_buf_helpers() {
        let mut buf = [0u8; 56];
        w_i32(&mut buf, 0, 0x1234);
        assert_eq!(r_i32(&buf, 0), 0x1234);

        w_u32(&mut buf, 4, 0xABCD);
        assert_eq!(r_u32(&buf, 4), 0xABCD);

        w_i64(&mut buf, 8, -0x1234567890ABCDEF);
        assert_eq!(r_i64(&buf, 8), -0x1234567890ABCDEF);

        w_u64(&mut buf, 16, 0xDEADBEEF);
        assert_eq!(r_u64(&buf, 16), 0xDEADBEEF);

        w_u16(&mut buf, 24, 0xBEEF);
        assert_eq!(r_u16(&buf, 24), 0xBEEF);
    }

    #[test]
    fn test_message_size() {
        // The MsgBuf must be exactly 56 bytes
        // (m_source=4, m_type=4, payload=48).
        assert_eq!(core::mem::size_of::<MsgBuf>(), 56);
    }

    #[test]
    fn test_req_vfsif_flags() {
        assert_eq!(REQ_RDONLY, 0o01);
        assert_eq!(REQ_ISROOT, 0o02);
        assert_eq!(PATH_GET_UCRED, 0o20);
    }
}
