//! `struct stat` / `struct statvfs` wire layouts.
//!
//! Every filesystem server answers STAT/STATVFS by copying one of these into
//! the caller's buffer through the grant VFS created, so the layout is shared
//! rather than per-filesystem (C: the shared `<sys/stat.h>` /
//! `<sys/statvfs.h>` headers).

/// File status — mirrors the userland `Stat` in `minix-std` (88 bytes on
/// 64-bit). Must stay byte-identical: it is written straight into the
/// caller's buffer through the grant created by VFS.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
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

/// Filesystem statistics — mirrors `Statvfs` in the VFS server.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Statvfs {
    pub f_flags: u64,
    pub f_bsize: u32,
    pub f_frsize: u32,
    pub f_blocks: u64,
    pub f_bfree: u64,
    pub f_bavail: u64,
    pub f_files: u64,
    pub f_ffree: u64,
    pub f_favail: u64,
    pub f_fsid: u64,
    pub f_flag: u64,
    pub f_namemax: u64,
}
