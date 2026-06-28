//! VFS request layer — FS communication wrappers.
//!
//! Each function in this module builds an FS request message, sends it to the
//! appropriate FS server (MFS, ext2, PFS, etc.) via IPC, and parses the
//! response.  The real implementations require grant/copy infrastructure and
//! the IPC message type definitions; for now every function is a stub that
//! returns `ENOSYS`.
//!
//! Ported from `minix/servers/vfs/request.c`.

use crate::vfs::consts::ENOSYS;
use crate::vfs::types::{LookupRes, NodeDetails, Statvfs, off_t};

// ── helpers ────────────────────────────────────────────────────────────────

/// Low-level IPC send/recv with a FS server.
///
/// # Safety
///
/// `msg` must point to a valid message buffer.  This is a stub that returns
/// `ENOSYS` until the IPC infrastructure is wired.
pub unsafe fn fs_sendrec(_fs_e: i32, _msg: *mut u8) -> i32 {
    ENOSYS
}

// ── Block-oriented operations ──────────────────────────────────────────────

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
    _fs_e: i32,
    _user_e: i32,
    _dev: u32,
    _pos: off_t,
    _num_of_bytes: u32,
    _user_addr: *const u8,
    _rw_flag: i32,
) -> (i32, off_t, u32) {
    (ENOSYS, 0, 0)
}

/// Block peek — query readable bytes at a device position without consuming.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_bpeek(_fs_e: i32, _dev: u32, _pos: off_t, _num_of_bytes: u32) -> i32 {
    ENOSYS
}

// ── Inode metadata operations ──────────────────────────────────────────────

/// Change mode of an inode.
///
/// Returns `(status, new_mode)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_chmod(_fs_e: i32, _inode_nr: u32, _rmode: u32) -> (i32, u32) {
    (ENOSYS, 0)
}

/// Change owner of an inode.
///
/// Returns `(status, new_mode)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_chown(_fs_e: i32, _inode_nr: u32, _newuid: u16, _newgid: u16) -> (i32, u32) {
    (ENOSYS, 0)
}

// ── File creation / destruction ────────────────────────────────────────────

/// Create a file.
///
/// Returns `(status, node_details)`.
///
/// # Safety
///
/// `path` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_create(
    _fs_e: i32,
    _inode_nr: u32,
    _omode: i32,
    _uid: u16,
    _gid: u16,
    _path: *const u8,
) -> (i32, NodeDetails) {
    (ENOSYS, NodeDetails::default())
}

/// Flush a device.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_flush(_fs_e: i32, _dev: u32) -> i32 {
    ENOSYS
}

/// Get filesystem statistics (statvfs).
///
/// Returns `(status, statvfs)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_statvfs(_fs_e: i32) -> (i32, Statvfs) {
    (ENOSYS, Statvfs::default())
}

/// Truncate a file between `start` and `end`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_ftrunc(_fs_e: i32, _inode_nr: u32, _start: off_t, _end: off_t) -> i32 {
    ENOSYS
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
    _fs_e: i32,
    _inode_nr: u32,
    _pos: off_t,
    _buf: *mut u8,
    _size: usize,
    _direct: i32,
) -> (i32, off_t) {
    (ENOSYS, 0)
}

/// Inhibit read — tell the FS not to issue read-ahead.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_inhibread(_fs_e: i32, _inode_nr: u32) -> i32 {
    ENOSYS
}

// ── Link / unlink ──────────────────────────────────────────────────────────

/// Create a hard link.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_link(_fs_e: i32, _link_parent: u32, _lastc: *const u8, _linked_file: u32) -> i32 {
    ENOSYS
}

// ── Path lookup ────────────────────────────────────────────────────────────

/// Path lookup — resolve a path relative to `dir_ino`.
///
/// Returns `(status, lookup_result)`.
///
/// # Safety
///
/// `resolve` must point to a valid `lookup` structure.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_lookup(
    _fs_e: i32,
    _dir_ino: u32,
    _root_ino: u32,
    _uid: u16,
    _gid: u16,
    _resolve: *const u8,
) -> (i32, LookupRes) {
    (ENOSYS, LookupRes::default())
}

/// Create a directory.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_mkdir(
    _fs_e: i32,
    _inode_nr: u32,
    _lastc: *const u8,
    _uid: u16,
    _gid: u16,
    _dmode: u32,
) -> i32 {
    ENOSYS
}

/// Create a special file (device node).
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
#[allow(clippy::too_many_arguments)]
pub unsafe fn req_mknod(
    _fs_e: i32,
    _inode_nr: u32,
    _lastc: *const u8,
    _uid: u16,
    _gid: u16,
    _dmode: u32,
    _dev: u32,
) -> i32 {
    ENOSYS
}

/// Check whether an inode is a mount point.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_mountpoint(_fs_e: i32, _inode_nr: u32) -> i32 {
    ENOSYS
}

/// Create a new inode.
///
/// Returns `(status, node_details)`.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_newnode(
    _fs_e: i32,
    _uid: u16,
    _gid: u16,
    _dmode: u32,
    _dev: u32,
) -> (i32, NodeDetails) {
    (ENOSYS, NodeDetails::default())
}

/// Notify a FS about a new driver.
///
/// # Safety
///
/// `label` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_newdriver(_fs_e: i32, _dev: u32, _label: *const u8) -> i32 {
    ENOSYS
}

// ── Inode ref-counting ─────────────────────────────────────────────────────

/// Release an inode reference.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_putnode(_fs_e: i32, _inode_nr: u32, _count: i32) -> i32 {
    ENOSYS
}

// ── Readlink ───────────────────────────────────────────────────────────────

/// Read the target of a symbolic link.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `len` bytes.  Caller must
/// ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_rdlink(
    _fs_e: i32,
    _inode_nr: u32,
    _proc_e: i32,
    _buf: *mut u8,
    _len: usize,
    _direct: i32,
) -> i32 {
    ENOSYS
}

// ── Superblock ─────────────────────────────────────────────────────────────

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
    _vmp: *const u8,
    _label: *const u8,
    _dev: u32,
    _readonly: i32,
    _isroot: i32,
) -> (i32, NodeDetails, u32) {
    (ENOSYS, NodeDetails::default(), 0)
}

// ── Rename ─────────────────────────────────────────────────────────────────

/// Rename a file or directory.
///
/// # Safety
///
/// `old_name` and `new_name` must point to valid NUL-terminated strings.
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_rename(
    _fs_e: i32,
    _old_parent: u32,
    _old_name: *const u8,
    _new_parent: u32,
    _new_name: *const u8,
) -> i32 {
    ENOSYS
}

/// Remove a directory.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_rmdir(_fs_e: i32, _inode_nr: u32, _lastc: *const u8) -> i32 {
    ENOSYS
}

// ── Symlink ────────────────────────────────────────────────────────────────

/// Create a symbolic link.
///
/// # Safety
///
/// `lastc` and `path` must point to valid NUL-terminated strings.  Caller
/// must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_slink(
    _fs_e: i32,
    _inode_nr: u32,
    _lastc: *const u8,
    _uid: u16,
    _gid: u16,
    _path: *const u8,
) -> i32 {
    ENOSYS
}

// ── Stat ───────────────────────────────────────────────────────────────────

/// Stat an inode — write `struct stat` into a user-provided buffer.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `len` bytes.  Caller must
/// ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_stat(_fs_e: i32, _inode_nr: u32, _who_e: i32, _buf: *mut u8, _len: usize) -> i32 {
    ENOSYS
}

// ── Sync ───────────────────────────────────────────────────────────────────

/// Sync a filesystem.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_sync(_fs_e: i32) -> i32 {
    ENOSYS
}

// ── Unlink ─────────────────────────────────────────────────────────────────

/// Remove a (non-directory) name from a directory.
///
/// # Safety
///
/// `lastc` must point to a valid NUL-terminated string.  Caller must ensure
/// `fs_e` is a valid FS endpoint.
pub unsafe fn req_unlink(_fs_e: i32, _inode_nr: u32, _lastc: *const u8) -> i32 {
    ENOSYS
}

/// Unmount a filesystem.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_unmount(_fs_e: i32) -> i32 {
    ENOSYS
}

// ── Timestamps ─────────────────────────────────────────────────────────────

/// Set access and modification times of an inode.
///
/// # Safety
///
/// Caller must ensure `fs_e` is a valid FS endpoint.
pub unsafe fn req_utime(_fs_e: i32, _inode_nr: u32, _actime: off_t, _modtime: off_t) -> i32 {
    ENOSYS
}

// ── Read / Write (regular file oriented) ───────────────────────────────────

/// Write to a regular file.
///
/// Returns `(status, new_pos)`.
///
/// # Safety
///
/// `buf` must point to a valid buffer of at least `size` bytes.  Caller must
/// ensure `fs_e` and `user_e` are valid endpoints.
pub unsafe fn req_write(
    _fs_e: i32,
    _inode_nr: u32,
    _buf: *const u8,
    _pos: off_t,
    _size: u32,
    _user_e: i32,
    _direct: i32,
) -> (i32, off_t) {
    (ENOSYS, 0)
}
