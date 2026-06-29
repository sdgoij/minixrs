//! VFS file I/O protocol wrappers.
//!
//! Provides `open`, `close`, `read`, `write`, `lseek`, `fstat`, `ioctl`,
//! `getdents`, `fsync`, and `truncate` by sending VFS server messages
//! via the kernel IPC syscall.
//!
//! VFS call numbers (from `.refs/minix-3.3.0/minix/include/minix/callnr.h`):
//! ```text
//! VFS_BASE       = 0x100
//! VFS_READ       = VFS_BASE + 0   (0x100)
//! VFS_WRITE      = VFS_BASE + 1   (0x101)
//! VFS_LSEEK      = VFS_BASE + 2   (0x102)
//! VFS_OPEN       = VFS_BASE + 3   (0x103)
//! VFS_CREAT      = VFS_BASE + 4   (0x104)
//! VFS_CLOSE      = VFS_BASE + 5   (0x105)
//! VFS_STAT       = VFS_BASE + 21  (0x115)
//! VFS_FSTAT      = VFS_BASE + 22  (0x116)
//! VFS_IOCTL      = VFS_BASE + 24  (0x118)
//! VFS_GETDENTS   = VFS_BASE + 29  (0x11D)
//! VFS_SELECT     = VFS_BASE + 30  (0x11E)
//! VFS_FSYNC      = VFS_BASE + 32  (0x120)
//! VFS_TRUNCATE   = VFS_BASE + 33  (0x121)
//! VFS_COPYFD     = VFS_BASE + 46  (0x12E)
//! ```

#![allow(dead_code)]

#[cfg(target_os = "none")]
use crate::{Message, sendrec};
use crate::{MinixErr, VFS_PROC_NR};

// ═══════════════════════════════════════════════════════════════════════════
// VFS call numbers
// ═══════════════════════════════════════════════════════════════════════════

pub const VFS_BASE: u32 = 0x100;

pub const VFS_READ: u32 = VFS_BASE;
pub const VFS_WRITE: u32 = VFS_BASE + 1;
pub const VFS_LSEEK: u32 = VFS_BASE + 2;
pub const VFS_OPEN: u32 = VFS_BASE + 3;
pub const VFS_CREAT: u32 = VFS_BASE + 4;
pub const VFS_CLOSE: u32 = VFS_BASE + 5;
pub const VFS_STAT: u32 = VFS_BASE + 21;
pub const VFS_FSTAT: u32 = VFS_BASE + 22;
pub const VFS_IOCTL: u32 = VFS_BASE + 24;
pub const VFS_GETDENTS: u32 = VFS_BASE + 29;
pub const VFS_SELECT: u32 = VFS_BASE + 30;
pub const VFS_FSYNC: u32 = VFS_BASE + 32;
pub const VFS_TRUNCATE: u32 = VFS_BASE + 33;
pub const VFS_COPYFD: u32 = VFS_BASE + 46;

// ═══════════════════════════════════════════════════════════════════════════
// Open flags  (from `minix/include/fcntl.h`)
// ═══════════════════════════════════════════════════════════════════════════

pub const O_RDONLY: i32 = 0o00;
pub const O_WRONLY: i32 = 0o01;
pub const O_RDWR: i32 = 0o02;
pub const O_CREAT: i32 = 0o100;
pub const O_EXCL: i32 = 0o200;
pub const O_NOCTTY: i32 = 0o400;
pub const O_TRUNC: i32 = 0o1000;
pub const O_APPEND: i32 = 0o2000;
pub const O_NONBLOCK: i32 = 0o4000;
pub const O_SYNC: i32 = 0o10000;

// ═══════════════════════════════════════════════════════════════════════════
// Seek whence constants
// ═══════════════════════════════════════════════════════════════════════════

pub const SEEK_SET: i32 = 0;
pub const SEEK_CUR: i32 = 1;
pub const SEEK_END: i32 = 2;

// ═══════════════════════════════════════════════════════════════════════════
// File type constants (for st_mode in Stat)
// ═══════════════════════════════════════════════════════════════════════════

pub const S_IFMT: u32 = 0o170000;
pub const S_IFSOCK: u32 = 0o140000;
pub const S_IFLNK: u32 = 0o120000;
pub const S_IFREG: u32 = 0o100000;
pub const S_IFBLK: u32 = 0o060000;
pub const S_IFDIR: u32 = 0o040000;
pub const S_IFCHR: u32 = 0o020000;
pub const S_IFIFO: u32 = 0o010000;

// ═══════════════════════════════════════════════════════════════════════════
// Stat structure
// ═══════════════════════════════════════════════════════════════════════════

/// File status structure (mirrors POSIX `stat`).
#[repr(C)]
pub struct Stat {
    pub st_dev: u64,
    pub st_ino: u64,
    pub st_mode: u32,
    pub st_nlink: u32,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
    pub st_size: i64,
    pub st_blksize: i64,
    pub st_blocks: i64,
    pub st_atime: i64,
    pub st_mtime: i64,
    pub st_ctime: i64,
}

// ═══════════════════════════════════════════════════════════════════════════
// Message field offsets (64-byte message buffer)
// ═══════════════════════════════════════════════════════════════════════════

// For VFS calls, the message layout is:
//   offset 0:  dest endpoint (i32) — set by sendrec
//   offset 4:  source endpoint (i32) — set by kernel
//   offset 8:  call_nr / m_type (i32) — VFS_* constant / return value
//   offset 12+: call-specific data

const OFF_TYPE: usize = 8;

// ── VFS_OPEN / VFS_CREAT ──────────────────────────────────────────────────
//   offset 12: name pointer (u64)
//   offset 20: name length (i32)
//   offset 24: flags (i32)   — O_RDONLY, O_WRONLY, etc.
//   offset 28: mode (i32)    — only for VFS_CREAT

const OFF_OPEN_NAME: usize = 12;
const OFF_OPEN_NAME_LEN: usize = 20;
const OFF_OPEN_FLAGS: usize = 24;
const OFF_OPEN_MODE: usize = 28;

// ── VFS_READ / VFS_WRITE ──────────────────────────────────────────────────
//   offset 12: fd (i32)
//   offset 16: buf pointer (u64)
//   offset 24: nbytes (u64)   — number of bytes
//   offset 32: position (u64) — seek position (for pread/pwrite), or 0

const OFF_RW_FD: usize = 12;
const OFF_RW_BUF: usize = 16;
const OFF_RW_NBYTES: usize = 24;
const OFF_RW_POSITION: usize = 32;

// ── VFS_CLOSE ─────────────────────────────────────────────────────────────
//   offset 12: fd (i32)

const OFF_CLOSE_FD: usize = 12;

// ── VFS_LSEEK ─────────────────────────────────────────────────────────────
//   offset 12: fd (i32)
//   offset 16: offset (i64)
//   offset 24: whence (i32)

const OFF_LSEEK_FD: usize = 12;
const OFF_LSEEK_OFFSET: usize = 16;
const OFF_LSEEK_WHENCE: usize = 24;

// ── VFS_STAT / VFS_FSTAT ──────────────────────────────────────────────────
//   offset 12: name/fd (i32) — fd for fstat, name pointer for stat
//   offset 16: stat buffer (u64) — pointer to stat struct in caller's space

const OFF_STAT_NAME_FD: usize = 12;
const OFF_STAT_BUF: usize = 16;

// ── VFS_IOCTL ─────────────────────────────────────────────────────────────
//   offset 12: fd (i32)
//   offset 16: request (u32) — ioctl request code
//   offset 20: arg pointer (u64)

const OFF_IOCTL_FD: usize = 12;
const OFF_IOCTL_REQ: usize = 16;
const OFF_IOCTL_ARG: usize = 20;

// ── VFS_GETDENTS ──────────────────────────────────────────────────────────
//   offset 12: fd (i32)
//   offset 16: buf pointer (u64)
//   offset 24: nbytes (u64)

const OFF_GETDENTS_FD: usize = 12;
const OFF_GETDENTS_BUF: usize = 16;
const OFF_GETDENTS_NBYTES: usize = 24;

// ── VFS_SELECT ────────────────────────────────────────────────────────────
//   offset 12: nfds (i32)
//   offset 16: fd_set pointer (u64) — readfds
//   offset 24: fd_set pointer (u64) — writefds

const OFF_SELECT_NFDS: usize = 12;
const OFF_SELECT_READFDS: usize = 16;
const OFF_SELECT_WRITEFDS: usize = 24;

// ── VFS_FSYNC / VFS_TRUNCATE ──────────────────────────────────────────────
//   offset 12: fd (i32)
//   offset 16: length (i64) — only for VFS_TRUNCATE

const OFF_FD_ONLY: usize = 12;
const OFF_TRUNC_LENGTH: usize = 16;

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Read an i32 from a message buffer at the given offset.
fn msg_i32(msg: &[u8; 64], off: usize) -> i32 {
    i32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

/// Write an i32 into a message buffer at the given offset.
fn msg_set_i32(msg: &mut [u8; 64], off: usize, val: i32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

/// Read a u32 from a message buffer at the given offset.
fn msg_u32(msg: &[u8; 64], off: usize) -> u32 {
    u32::from_ne_bytes(msg[off..off + 4].try_into().unwrap())
}

/// Write a u32 into a message buffer at the given offset.
fn msg_set_u32(msg: &mut [u8; 64], off: usize, val: u32) {
    msg[off..off + 4].copy_from_slice(&val.to_ne_bytes());
}

/// Read a u64 from a message buffer at the given offset.
fn msg_u64(msg: &[u8; 64], off: usize) -> u64 {
    u64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

/// Write a u64 into a message buffer at the given offset.
fn msg_set_u64(msg: &mut [u8; 64], off: usize, val: u64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

/// Read an i64 from a message buffer at the given offset.
fn msg_i64(msg: &[u8; 64], off: usize) -> i64 {
    i64::from_ne_bytes(msg[off..off + 8].try_into().unwrap())
}

/// Write an i64 into a message buffer at the given offset.
fn msg_set_i64(msg: &mut [u8; 64], off: usize, val: i64) {
    msg[off..off + 8].copy_from_slice(&val.to_ne_bytes());
}

// ═══════════════════════════════════════════════════════════════════════════
// Internal: send a VFS request and check the result
// ═══════════════════════════════════════════════════════════════════════════

/// Perform a VFS `sendrec` and validate the response m_type.
///
/// Returns `Ok(m_type)` on success (m_type >= 0) or `Err(MinixErr)` when the
/// VFS server returned a negative error code.
#[cfg(target_os = "none")]
unsafe fn vfs_call(msg: &mut [u8; 64]) -> Result<i32, MinixErr> {
    sendrec(VFS_PROC_NR, msg)?;
    let mtype = msg_i32(msg, OFF_TYPE);
    if mtype < 0 {
        Err(MinixErr::from_i32(mtype))
    } else {
        Ok(mtype)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// File descriptor operations
// ═══════════════════════════════════════════════════════════════════════════

/// Open a file.
///
/// `path` is the null-terminated path string. `flags` is a bitwise OR of
/// `O_RDONLY`, `O_WRONLY`, `O_RDWR`, `O_CREAT`, `O_TRUNC`, `O_APPEND`, etc.
/// `mode` specifies the file permissions when `O_CREAT` is set.
///
/// Returns the file descriptor on success.
///
/// # Safety
///
/// `path` must be a valid, null-terminated string in the caller's address
/// space.
pub unsafe fn open(path: &str, flags: i32, mode: u32) -> Result<i32, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (path, flags, mode, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_OPEN as i32);
        msg_set_u64(&mut msg, OFF_OPEN_NAME, path.as_ptr() as u64);
        msg_set_i32(&mut msg, OFF_OPEN_NAME_LEN, (path.len() + 1) as i32);
        msg_set_i32(&mut msg, OFF_OPEN_FLAGS, flags);
        msg_set_u32(&mut msg, OFF_OPEN_MODE, mode);
        let mtype = vfs_call(&mut msg)?;
        // VFS_OPEN returns the file descriptor in m_type on success.
        Ok(mtype)
    }
}

/// Close a file descriptor.
pub fn close(fd: i32) -> Result<(), MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_CLOSE as i32);
        msg_set_i32(&mut msg, OFF_CLOSE_FD, fd);
        let _ = vfs_call(&mut msg)?;
        Ok(())
    }
}

/// Read from a file descriptor into a buffer.
///
/// Returns the number of bytes read, which may be less than `buf.len()`
/// (short read) or 0 at end of file.
///
/// # Safety
///
/// `buf` must be a valid, mutable byte slice in the caller's address space.
pub unsafe fn read(fd: i32, buf: &mut [u8]) -> Result<i64, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, buf, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_READ as i32);
        msg_set_i32(&mut msg, OFF_RW_FD, fd);
        msg_set_u64(&mut msg, OFF_RW_BUF, buf.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_RW_NBYTES, buf.len() as u64);
        msg_set_u64(&mut msg, OFF_RW_POSITION, 0); // not pread — position is 0
        let mtype = vfs_call(&mut msg)?;
        Ok(mtype as i64)
    }
}

/// Write to a file descriptor from a buffer.
///
/// Returns the number of bytes written, which may be less than `buf.len()`
/// (short write).
///
/// # Safety
///
/// `buf` must be a valid byte slice in the caller's address space.
pub unsafe fn write(fd: i32, buf: &[u8]) -> Result<i64, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, buf, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_WRITE as i32);
        msg_set_i32(&mut msg, OFF_RW_FD, fd);
        msg_set_u64(&mut msg, OFF_RW_BUF, buf.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_RW_NBYTES, buf.len() as u64);
        msg_set_u64(&mut msg, OFF_RW_POSITION, 0);
        let mtype = vfs_call(&mut msg)?;
        Ok(mtype as i64)
    }
}

/// Reposition the file offset for an open file descriptor.
///
/// `whence` is one of `SEEK_SET`, `SEEK_CUR`, or `SEEK_END`.
/// Returns the resulting file position relative to the beginning of the file.
pub fn lseek(fd: i32, offset: i64, whence: i32) -> Result<i64, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, offset, whence, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_LSEEK as i32);
        msg_set_i32(&mut msg, OFF_LSEEK_FD, fd);
        msg_set_i64(&mut msg, OFF_LSEEK_OFFSET, offset);
        msg_set_i32(&mut msg, OFF_LSEEK_WHENCE, whence);
        let mtype = vfs_call(&mut msg)?;
        Ok(mtype as i64)
    }
}

/// Get file status for an open file descriptor.
pub fn fstat(fd: i32) -> Result<Stat, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut stat_buf = core::mem::MaybeUninit::<Stat>::zeroed();
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_FSTAT as i32);
        msg_set_i32(&mut msg, OFF_STAT_NAME_FD, fd);
        msg_set_u64(&mut msg, OFF_STAT_BUF, stat_buf.as_mut_ptr() as u64);
        let _ = vfs_call(&mut msg)?;
        Ok(stat_buf.assume_init())
    }
}

/// Perform an I/O control operation on a file descriptor.
///
/// # Safety
///
/// `arg` must be a valid pointer to a buffer whose interpretation depends
/// on `request`.
pub unsafe fn ioctl(fd: i32, request: u32, arg: *mut u8) -> Result<i32, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, request, arg, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_IOCTL as i32);
        msg_set_i32(&mut msg, OFF_IOCTL_FD, fd);
        msg_set_u32(&mut msg, OFF_IOCTL_REQ, request);
        msg_set_u64(&mut msg, OFF_IOCTL_ARG, arg as u64);
        let mtype = vfs_call(&mut msg)?;
        Ok(mtype)
    }
}

/// Read directory entries from a file descriptor.
///
/// Fills `buf` with `struct dirent` entries. Returns the number of bytes
/// written into `buf`, or 0 at end of directory.
pub fn getdents(fd: i32, buf: &mut [u8]) -> Result<i32, MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, buf, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_GETDENTS as i32);
        msg_set_i32(&mut msg, OFF_GETDENTS_FD, fd);
        msg_set_u64(&mut msg, OFF_GETDENTS_BUF, buf.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_GETDENTS_NBYTES, buf.len() as u64);
        let mtype = vfs_call(&mut msg)?;
        Ok(mtype)
    }
}

/// Synchronize a file's in-core state with storage device.
pub fn fsync(fd: i32) -> Result<(), MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_FSYNC as i32);
        msg_set_i32(&mut msg, OFF_FD_ONLY, fd);
        let _ = vfs_call(&mut msg)?;
        Ok(())
    }
}

/// Truncate a file to a specified length.
pub fn truncate(fd: i32, length: i64) -> Result<(), MinixErr> {
    #[cfg(not(target_os = "none"))]
    {
        let _ = (fd, length, VFS_PROC_NR);
        Err(MinixErr::ENOSYS)
    }
    #[cfg(target_os = "none")]
    unsafe {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_TRUNCATE as i32);
        msg_set_i32(&mut msg, OFF_FD_ONLY, fd);
        msg_set_i64(&mut msg, OFF_TRUNC_LENGTH, length);
        let _ = vfs_call(&mut msg)?;
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vfs_call_numbers() {
        assert_eq!(VFS_BASE, 0x100);
        assert_eq!(VFS_READ, 0x100);
        assert_eq!(VFS_WRITE, 0x101);
        assert_eq!(VFS_LSEEK, 0x102);
        assert_eq!(VFS_OPEN, 0x103);
        assert_eq!(VFS_CREAT, 0x104);
        assert_eq!(VFS_CLOSE, 0x105);
        assert_eq!(VFS_STAT, 0x115);
        assert_eq!(VFS_FSTAT, 0x116);
        assert_eq!(VFS_IOCTL, 0x118);
        assert_eq!(VFS_GETDENTS, 0x11D);
        assert_eq!(VFS_SELECT, 0x11E);
        assert_eq!(VFS_FSYNC, 0x120);
        assert_eq!(VFS_TRUNCATE, 0x121);
        assert_eq!(VFS_COPYFD, 0x12E);
    }

    #[test]
    fn test_open_flags() {
        assert_eq!(O_RDONLY, 0o00);
        assert_eq!(O_WRONLY, 0o01);
        assert_eq!(O_RDWR, 0o02);
        assert_eq!(O_CREAT, 0o100);
        assert_eq!(O_EXCL, 0o200);
        assert_eq!(O_NOCTTY, 0o400);
        assert_eq!(O_TRUNC, 0o1000);
        assert_eq!(O_APPEND, 0o2000);
        assert_eq!(O_NONBLOCK, 0o4000);
        assert_eq!(O_SYNC, 0o10000);
    }

    #[test]
    fn test_seek_constants() {
        assert_eq!(SEEK_SET, 0);
        assert_eq!(SEEK_CUR, 1);
        assert_eq!(SEEK_END, 2);
    }

    #[test]
    fn test_file_type_constants() {
        assert_eq!(S_IFMT, 0o170000);
        assert_eq!(S_IFSOCK, 0o140000);
        assert_eq!(S_IFLNK, 0o120000);
        assert_eq!(S_IFREG, 0o100000);
        assert_eq!(S_IFBLK, 0o060000);
        assert_eq!(S_IFDIR, 0o040000);
        assert_eq!(S_IFCHR, 0o020000);
        assert_eq!(S_IFIFO, 0o010000);
    }

    #[test]
    fn test_stat_struct_layout() {
        // Verify that Stat has the expected size and field offsets.
        // The struct is repr(C), so fields are laid out in declaration order.
        // On a 64-bit target with repr(C), each field starts at a multiple of
        // its alignment.
        // On 64-bit: 13 fields, all 8-byte aligned except three u32s packed at
        // offsets 16-31 (st_mode, st_nlink, st_uid, st_gid = 16 bytes).
        assert_eq!(core::mem::size_of::<Stat>(), 88);

        // Construct a zero-initialized Stat and verify access.
        let st: Stat = unsafe { core::mem::zeroed() };
        assert_eq!(st.st_dev, 0);
        assert_eq!(st.st_ino, 0);
        assert_eq!(st.st_mode, 0);
        assert_eq!(st.st_nlink, 0);
        assert_eq!(st.st_uid, 0);
        assert_eq!(st.st_gid, 0);
        assert_eq!(st.st_rdev, 0);
        assert_eq!(st.st_size, 0);
        assert_eq!(st.st_blksize, 0);
        assert_eq!(st.st_blocks, 0);
        assert_eq!(st.st_atime, 0);
        assert_eq!(st.st_mtime, 0);
        assert_eq!(st.st_ctime, 0);
    }

    #[test]
    fn test_open_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_OPEN as i32);
        msg_set_u64(&mut msg, OFF_OPEN_NAME, 0x1234_5678_9ABC_DEF0);
        msg_set_i32(&mut msg, OFF_OPEN_NAME_LEN, 5); // e.g. "/tmp\0"
        msg_set_i32(&mut msg, OFF_OPEN_FLAGS, O_RDWR | O_CREAT);
        msg_set_u32(&mut msg, OFF_OPEN_MODE, 0o644);

        assert_eq!(msg_i32(&msg, 8), 0x103);
        assert_eq!(msg_u64(&msg, 12), 0x1234_5678_9ABC_DEF0);
        assert_eq!(msg_i32(&msg, 20), 5);
        assert_eq!(msg_i32(&msg, 24), O_RDWR | O_CREAT);
        assert_eq!(msg_u32(&msg, 28), 0o644);
    }

    #[test]
    fn test_read_message_format() {
        let mut msg = [0u8; 64];
        let buf: [u8; 128] = [0; 128];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_READ as i32);
        msg_set_i32(&mut msg, OFF_RW_FD, 3);
        msg_set_u64(&mut msg, OFF_RW_BUF, buf.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_RW_NBYTES, 128);
        msg_set_u64(&mut msg, OFF_RW_POSITION, 0);

        assert_eq!(msg_i32(&msg, 8), 0x100);
        assert_eq!(msg_i32(&msg, 12), 3);
        assert_eq!(msg_u64(&msg, 16), buf.as_ptr() as u64);
        assert_eq!(msg_u64(&msg, 24), 128);
        assert_eq!(msg_u64(&msg, 32), 0);
    }

    #[test]
    fn test_write_message_format() {
        let mut msg = [0u8; 64];
        let buf: [u8; 64] = [0; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_WRITE as i32);
        msg_set_i32(&mut msg, OFF_RW_FD, 1);
        msg_set_u64(&mut msg, OFF_RW_BUF, buf.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_RW_NBYTES, 64);
        msg_set_u64(&mut msg, OFF_RW_POSITION, 0);

        assert_eq!(msg_i32(&msg, 8), 0x101);
        assert_eq!(msg_i32(&msg, 12), 1);
        assert_eq!(msg_u64(&msg, 16), buf.as_ptr() as u64);
        assert_eq!(msg_u64(&msg, 24), 64);
    }

    #[test]
    fn test_close_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_CLOSE as i32);
        msg_set_i32(&mut msg, OFF_CLOSE_FD, 42);

        assert_eq!(msg_i32(&msg, 8), 0x105);
        assert_eq!(msg_i32(&msg, 12), 42);
    }

    #[test]
    fn test_lseek_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_LSEEK as i32);
        msg_set_i32(&mut msg, OFF_LSEEK_FD, 3);
        msg_set_i64(&mut msg, OFF_LSEEK_OFFSET, 100);
        msg_set_i32(&mut msg, OFF_LSEEK_WHENCE, SEEK_SET);

        assert_eq!(msg_i32(&msg, 8), 0x102);
        assert_eq!(msg_i32(&msg, 12), 3);
        assert_eq!(msg_i64(&msg, 16), 100);
        assert_eq!(msg_i32(&msg, 24), SEEK_SET);
    }

    #[test]
    fn test_fstat_message_format() {
        let mut msg = [0u8; 64];
        let stat_buf: Stat = unsafe { core::mem::zeroed() };
        msg_set_i32(&mut msg, OFF_TYPE, VFS_FSTAT as i32);
        msg_set_i32(&mut msg, OFF_STAT_NAME_FD, 3);
        msg_set_u64(&mut msg, OFF_STAT_BUF, &stat_buf as *const Stat as u64);

        assert_eq!(msg_i32(&msg, 8), 0x116);
        assert_eq!(msg_i32(&msg, 12), 3);
        assert_eq!(msg_u64(&msg, 16), &stat_buf as *const Stat as u64);
    }

    #[test]
    fn test_ioctl_message_format() {
        let mut msg = [0u8; 64];
        let arg: u8 = 0;
        msg_set_i32(&mut msg, OFF_TYPE, VFS_IOCTL as i32);
        msg_set_i32(&mut msg, OFF_IOCTL_FD, 3);
        msg_set_u32(&mut msg, OFF_IOCTL_REQ, 0x1234_5678);
        msg_set_u64(&mut msg, OFF_IOCTL_ARG, &arg as *const u8 as u64);

        assert_eq!(msg_i32(&msg, 8), 0x118);
        assert_eq!(msg_i32(&msg, 12), 3);
        assert_eq!(msg_u32(&msg, 16), 0x1234_5678);
        assert_eq!(msg_u64(&msg, 20), &arg as *const u8 as u64);
    }

    #[test]
    fn test_getdents_message_format() {
        let mut msg = [0u8; 64];
        let buf: [u8; 256] = [0; 256];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_GETDENTS as i32);
        msg_set_i32(&mut msg, OFF_GETDENTS_FD, 3);
        msg_set_u64(&mut msg, OFF_GETDENTS_BUF, buf.as_ptr() as u64);
        msg_set_u64(&mut msg, OFF_GETDENTS_NBYTES, 256);

        assert_eq!(msg_i32(&msg, 8), 0x11D);
        assert_eq!(msg_i32(&msg, 12), 3);
        assert_eq!(msg_u64(&msg, 16), buf.as_ptr() as u64);
        assert_eq!(msg_u64(&msg, 24), 256);
    }

    #[test]
    fn test_fsync_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_FSYNC as i32);
        msg_set_i32(&mut msg, OFF_FD_ONLY, 3);

        assert_eq!(msg_i32(&msg, 8), 0x120);
        assert_eq!(msg_i32(&msg, 12), 3);
    }

    #[test]
    fn test_truncate_message_format() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, OFF_TYPE, VFS_TRUNCATE as i32);
        msg_set_i32(&mut msg, OFF_FD_ONLY, 3);
        msg_set_i64(&mut msg, OFF_TRUNC_LENGTH, 1024);

        assert_eq!(msg_i32(&msg, 8), 0x121);
        assert_eq!(msg_i32(&msg, 12), 3);
        assert_eq!(msg_i64(&msg, 16), 1024);
    }

    #[test]
    fn test_msg_helpers() {
        let mut msg = [0u8; 64];
        msg_set_i32(&mut msg, 0, -1);
        assert_eq!(msg_i32(&msg, 0), -1);

        msg_set_u32(&mut msg, 4, 0xDEAD_BEEF);
        assert_eq!(msg_u32(&msg, 4), 0xDEAD_BEEF);

        msg_set_u64(&mut msg, 8, 0x1234_5678_9ABC_DEF0);
        assert_eq!(msg_u64(&msg, 8), 0x1234_5678_9ABC_DEF0);

        msg_set_i64(&mut msg, 16, -123456789012345);
        assert_eq!(msg_i64(&msg, 16), -123456789012345);
    }

    #[test]
    fn test_open_returns_enosys_on_host() {
        let result = unsafe { open("/test", O_RDONLY, 0) };
        assert!(result.is_err());
    }

    #[test]
    fn test_close_returns_enosys_on_host() {
        let result = close(0);
        assert!(result.is_err());
    }

    #[test]
    fn test_read_returns_enosys_on_host() {
        let result = unsafe { read(0, &mut []) };
        assert!(result.is_err());
    }

    #[test]
    fn test_write_returns_enosys_on_host() {
        let result = unsafe { write(0, &[]) };
        assert!(result.is_err());
    }

    #[test]
    fn test_lseek_returns_enosys_on_host() {
        let result = lseek(0, 0, SEEK_SET);
        assert!(result.is_err());
    }

    #[test]
    fn test_fstat_returns_enosys_on_host() {
        let result = fstat(0);
        assert!(result.is_err());
    }

    #[test]
    fn test_ioctl_returns_enosys_on_host() {
        let result = unsafe { ioctl(0, 0, core::ptr::null_mut()) };
        assert!(result.is_err());
    }

    #[test]
    fn test_getdents_returns_enosys_on_host() {
        let result = getdents(0, &mut []);
        assert!(result.is_err());
    }

    #[test]
    fn test_fsync_returns_enosys_on_host() {
        let result = fsync(0);
        assert!(result.is_err());
    }

    #[test]
    fn test_truncate_returns_enosys_on_host() {
        let result = truncate(0, 0);
        assert!(result.is_err());
    }

    #[test]
    fn test_open_signature() {
        fn _check(f: unsafe fn(&str, i32, u32) -> Result<i32, MinixErr>) {
            let _ = f;
        }
        _check(open);
    }

    #[test]
    fn test_close_signature() {
        fn _check(f: fn(i32) -> Result<(), MinixErr>) {
            let _ = f;
        }
        _check(close);
    }

    #[test]
    fn test_read_signature() {
        fn _check(f: unsafe fn(i32, &mut [u8]) -> Result<i64, MinixErr>) {
            let _ = f;
        }
        _check(read);
    }

    #[test]
    fn test_write_signature() {
        fn _check(f: unsafe fn(i32, &[u8]) -> Result<i64, MinixErr>) {
            let _ = f;
        }
        _check(write);
    }

    #[test]
    fn test_lseek_signature() {
        fn _check(f: fn(i32, i64, i32) -> Result<i64, MinixErr>) {
            let _ = f;
        }
        _check(lseek);
    }

    #[test]
    fn test_fstat_signature() {
        fn _check(f: fn(i32) -> Result<Stat, MinixErr>) {
            let _ = f;
        }
        _check(fstat);
    }

    #[test]
    fn test_ioctl_signature() {
        fn _check(f: unsafe fn(i32, u32, *mut u8) -> Result<i32, MinixErr>) {
            let _ = f;
        }
        _check(ioctl);
    }

    #[test]
    fn test_getdents_signature() {
        fn _check(f: fn(i32, &mut [u8]) -> Result<i32, MinixErr>) {
            let _ = f;
        }
        _check(getdents);
    }

    #[test]
    fn test_fsync_signature() {
        fn _check(f: fn(i32) -> Result<(), MinixErr>) {
            let _ = f;
        }
        _check(fsync);
    }

    #[test]
    fn test_truncate_signature() {
        fn _check(f: fn(i32, i64) -> Result<(), MinixErr>) {
            let _ = f;
        }
        _check(truncate);
    }
}
