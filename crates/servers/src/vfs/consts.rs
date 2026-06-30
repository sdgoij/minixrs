//! VFS constants — adapted from `minix/servers/vfs/const.h`
//! and `minix/include/minix/callnr.h`.

// Table sizes

/// Number of slots in the filp table.
pub const NR_FILPS: usize = 1024;

/// Number of slots in the file locking table.
pub const NR_LOCKS: usize = 8;

/// Number of slots in the mount table.
pub const NR_MNTS: usize = 16;

/// Number of slots in the vnode table.
pub const NR_VNODES: usize = 1024;

/// Number of worker threads.
pub const NR_WTHREADS: usize = 9;

/// Number of slots in the nonedev bitmap.
pub const NR_NONEDEVS: usize = NR_MNTS;

// Open file limits

/// Maximum number of open files per process.
pub const OPEN_MAX: usize = 64;

// Path limits

/// Maximum path length.
pub const PATH_MAX: usize = 1024;

/// Maximum filename length.
pub const PNAME_MAX: usize = 255;

/// Maximum label size (including NUL terminator).
pub const LABEL_MAX: usize = 16;

// Device limits

/// Number of devices per driver.
pub const DEV_PER_DRIVER: usize = 256;

/// Number of device table slots.
pub const NR_DEVICES: usize = 64;

// Misc constants

/// Vnode lock types for the three-level lock (tll).
pub const VNODE_READ: i32 = 0;
pub const VNODE_WRITE: i32 = 1;
pub const VNODE_OPCL: i32 = 2;

/// Vmnt lock types.
pub const VMNT_READ: i32 = 0;
pub const VMNT_WRITE: i32 = 1;

/// Super-user UID.
pub const SU_UID: u16 = 0;

/// UID for system processes and INIT.
pub const SYS_UID: u16 = 0;

/// GID for system processes and INIT.
pub const SYS_GID: u16 = 0;

/// Known-invalid thread ID.
pub const INVALID_THREAD: i32 = -1;

/// Maximum symlink traversals.
pub const SYMLOOP: i32 = 16;

/// Maximum file system type size.
pub const FSTYPE_MAX: usize = 16; // VFS_NAMELEN

/// Process name length.
pub const PROC_NAME_LEN: usize = 16;

// FP_BLOCKED_ON constants

/// Not blocked.
pub const FP_BLOCKED_ON_NONE: i32 = 0;

/// Suspended on pipe.
pub const FP_BLOCKED_ON_PIPE: i32 = 1;

/// Suspended on lock.
pub const FP_BLOCKED_ON_LOCK: i32 = 2;

/// Suspended on pipe open.
pub const FP_BLOCKED_ON_POPEN: i32 = 3;

/// Suspended on select.
pub const FP_BLOCKED_ON_SELECT: i32 = 4;

/// Blocked on other process (check fp_task).
pub const FP_BLOCKED_ON_OTHER: i32 = 5;

// Fproc flags (fp_flags)

/// No flags.
pub const FP_NOFLAGS: u32 = 0x0000;

/// Set if process is a service.
pub const FP_SRV_PROC: u32 = 0x0001;

/// Indicates process is being revived.
pub const FP_REVIVED: u32 = 0x0002;

/// Set if process is session leader.
pub const FP_SESLDR: u32 = 0x0004;

/// Set if process has pending work.
pub const FP_PENDING: u32 = 0x0010;

/// Set if process is exiting.
pub const FP_EXITING: u32 = 0x0020;

/// Set if process has a postponed PM request.
pub const FP_PM_WORK: u32 = 0x0040;

// Reviving constants

/// Process is not being revived.
pub const NOT_REVIVING: i32 = 0xC0FFEEE;

/// Process is being revived from suspension.
pub const REVIVING: i32 = 0xDEEAD;

/// Process slot free.
pub const PID_FREE: i32 = 0;

// Filp constants

/// filp_mode: associated device closed/gone.
pub const FILP_CLOSED: u32 = 0;

/// The driver should be informed about new state.
pub const FSF_UPDATE: u32 = 0x01;

/// Select operation sent to driver but no reply yet.
pub const FSF_BUSY: u32 = 0x02;

/// Read request is blocking, driver should keep state.
pub const FSF_RD_BLOCK: u32 = 0x10;

/// Write request is blocking.
pub const FSF_WR_BLOCK: u32 = 0x20;

/// Exception request is blocking.
pub const FSF_ERR_BLOCK: u32 = 0x40;

/// Mask of all blocking flags.
pub const FSF_BLOCKED: u32 = 0x70;

// Vmnt flags

/// Device mounted readonly.
pub const VMNT_READONLY: u32 = 0x01;

/// FS did back call.
pub const VMNT_CALLBACK: u32 = 0x02;

/// Device is being mounted.
pub const VMNT_MOUNTING: u32 = 0x04;

/// Force usage of none-device.
pub const VMNT_FORCEROOTBSF: u32 = 0x08;

/// Include FS in getvfsstat output.
pub const VMNT_CANSTAT: u32 = 0x10;

// Select operation types

pub const SEL_RD: u32 = 0x01;
pub const SEL_WR: u32 = 0x02;
pub const SEL_ERR: u32 = 0x04;
pub const SEL_NOTIFY: u32 = 0x08;

// Misc

/// Number of boot processes.
pub const NR_BOOT_PROCS: usize = 32;

/// Maximum number of supplemental groups.
pub const NGROUPS_MAX: usize = 64;

// VFS call number constants (from callnr.h)

/// VFS call number base.
pub const VFS_BASE: i32 = 0x100;

/// Number of VFS calls.
pub const NR_VFS_CALLS: usize = 49;

pub const VFS_READ: i32 = VFS_BASE;
pub const VFS_WRITE: i32 = VFS_BASE + 1;
pub const VFS_LSEEK: i32 = VFS_BASE + 2;
pub const VFS_OPEN: i32 = VFS_BASE + 3;
pub const VFS_CREAT: i32 = VFS_BASE + 4;
pub const VFS_CLOSE: i32 = VFS_BASE + 5;
pub const VFS_LINK: i32 = VFS_BASE + 6;
pub const VFS_UNLINK: i32 = VFS_BASE + 7;
pub const VFS_CHDIR: i32 = VFS_BASE + 8;
pub const VFS_MKDIR: i32 = VFS_BASE + 9;
pub const VFS_MKNOD: i32 = VFS_BASE + 10;
pub const VFS_CHMOD: i32 = VFS_BASE + 11;
pub const VFS_CHOWN: i32 = VFS_BASE + 12;
pub const VFS_MOUNT: i32 = VFS_BASE + 13;
pub const VFS_UMOUNT: i32 = VFS_BASE + 14;
pub const VFS_ACCESS: i32 = VFS_BASE + 15;
pub const VFS_SYNC: i32 = VFS_BASE + 16;
pub const VFS_RENAME: i32 = VFS_BASE + 17;
pub const VFS_RMDIR: i32 = VFS_BASE + 18;
pub const VFS_SYMLINK: i32 = VFS_BASE + 19;
pub const VFS_READLINK: i32 = VFS_BASE + 20;
pub const VFS_STAT: i32 = VFS_BASE + 21;
pub const VFS_FSTAT: i32 = VFS_BASE + 22;
pub const VFS_LSTAT: i32 = VFS_BASE + 23;
pub const VFS_IOCTL: i32 = VFS_BASE + 24;
pub const VFS_FCNTL: i32 = VFS_BASE + 25;
pub const VFS_PIPE2: i32 = VFS_BASE + 26;
pub const VFS_UMASK: i32 = VFS_BASE + 27;
pub const VFS_CHROOT: i32 = VFS_BASE + 28;
pub const VFS_GETDENTS: i32 = VFS_BASE + 29;
pub const VFS_SELECT: i32 = VFS_BASE + 30;
pub const VFS_FCHDIR: i32 = VFS_BASE + 31;
pub const VFS_FSYNC: i32 = VFS_BASE + 32;
pub const VFS_TRUNCATE: i32 = VFS_BASE + 33;
pub const VFS_FTRUNCATE: i32 = VFS_BASE + 34;
pub const VFS_FCHMOD: i32 = VFS_BASE + 35;
pub const VFS_FCHOWN: i32 = VFS_BASE + 36;
pub const VFS_UTIMENS: i32 = VFS_BASE + 37;
pub const VFS_VMCALL: i32 = VFS_BASE + 38;
pub const VFS_GETVFSSTAT: i32 = VFS_BASE + 39;
pub const VFS_STATVFS1: i32 = VFS_BASE + 40;
pub const VFS_FSTATVFS1: i32 = VFS_BASE + 41;
pub const VFS_GETRUSAGE: i32 = VFS_BASE + 42;
pub const VFS_SVRCTL: i32 = VFS_BASE + 43;
pub const VFS_GCOV_FLUSH: i32 = VFS_BASE + 44;
pub const VFS_MAPDRIVER: i32 = VFS_BASE + 45;
pub const VFS_COPYFD: i32 = VFS_BASE + 46;
pub const VFS_CHECKPERMS: i32 = VFS_BASE + 47;
pub const VFS_GETSYSINFO: i32 = VFS_BASE + 48;

// Errno constants

/// F_DUPFD = 0 — duplicate fd as lowest available >= arg.
pub const F_DUPFD: i32 = 0;
/// F_GETFD = 1 — get close-on-exec flag.
pub const F_GETFD: i32 = 1;
/// F_SETFD = 2 — set close-on-exec flag.
pub const F_SETFD: i32 = 2;

pub const OK: i32 = 0;
pub const EPERM: i32 = -1;
pub const ENOENT: i32 = -2;
pub const ESRCH: i32 = -3;
pub const EINTR: i32 = -4;
pub const EIO: i32 = -5;
pub const ENXIO: i32 = -6;
pub const E2BIG: i32 = -7;
pub const EBADF: i32 = -9;
pub const EAGAIN: i32 = -11;
pub const ENOMEM: i32 = -12;
pub const EACCES: i32 = -13;
pub const EFAULT: i32 = -14;
pub const EBUSY: i32 = -16;
pub const EEXIST: i32 = -17;
pub const EXDEV: i32 = -18;
pub const ENODEV: i32 = -19;
pub const ENOTDIR: i32 = -20;
pub const EISDIR: i32 = -21;
pub const EINVAL: i32 = -22;
pub const ENFILE: i32 = -23;
pub const EMFILE: i32 = -24;
pub const ENOTTY: i32 = -25;
pub const EFBIG: i32 = -27;
pub const ENOSPC: i32 = -28;
pub const EROFS: i32 = -30;
pub const EMLINK: i32 = -31;
pub const EPIPE: i32 = -32;
pub const ELOOP: i32 = -40;
pub const ENAMETOOLONG: i32 = -36;
pub const ENOTEMPTY: i32 = -39;
pub const ENOSYS: i32 = -78;
pub const ENOTSOCK: i32 = -88;
pub const EOPNOTSUPP: i32 = -95;
pub const ECONNRESET: i32 = -104;
pub const ESYMLINK: i32 = -105;
pub const EENTERMOUNT: i32 = -106;
pub const ELEAVEMOUNT: i32 = -107;
pub const EDEADLK: i32 = -36;
pub const EWOULDBLOCK: i32 = EAGAIN;

// SUSPEND (internal code)

pub const SUSPEND: i32 = -998;

// Helper

/// Test if a process is blocked on something.
#[inline]
pub fn fp_is_blocked(fp_flags: i32) -> bool {
    fp_flags != FP_BLOCKED_ON_NONE
}

/// PFS (Pipe File System) endpoint number.
pub const PFS_PROC_NR: i32 = 4;

/// VM (memory server) endpoint number.
pub const VM_PROC_NR: i32 = 8;

/// VM↔VFS protocol (from `<minix/com.h>`).
pub const VMVFSREQ_FDLOOKUP: i32 = 101;
pub const VMVFSREQ_FDCLOSE: i32 = 102;
pub const VMVFSREQ_FDIO: i32 = 103;
pub const VM_RQ_BASE: i32 = 0xC00;
pub const VM_VFS_REPLY: i32 = VM_RQ_BASE + 30;

/// VMCALL message field offsets (M10 layout in 64-byte fs_m_in).
/// m10_i1 at byte 0, m10_i2 at byte 4, m10_i3 at byte 8, m10_i4 at byte 12
/// m10_ull1 at byte 16, m10_l2 at byte 24, m10_l3 at byte 32
pub const VMCALL_REQ_OFF: usize = 0;
pub const VMCALL_FD_OFF: usize = 4;
pub const VMCALL_REQID_OFF: usize = 8;
pub const VMCALL_ENDPOINT_OFF: usize = 12;
pub const VMCALL_OFFSET_OFF: usize = 16;
pub const VMCALL_LENGTH_OFF: usize = 24;

/// VM↔VFS reply field offsets (M10 layout in 64-byte fs_m_out).
pub const VMV_ENDPOINT_OFF: usize = 0;
pub const VMV_RESULT_OFF: usize = 4;
pub const VMV_REQID_OFF: usize = 8;
pub const VMV_DEV_OFF: usize = 12;
pub const VMV_INO_OFF: usize = 16;
pub const VMV_FD_OFF: usize = 24;
pub const VMV_SIZE_PAGES_OFF: usize = 32;

/// Named pipe mode for req_newnode.
pub const I_NAMED_PIPE: u32 = 0o010000;

/// CDEV message base (from `<minix/com.h>`).
pub const CDEV_RQ_BASE: i32 = 0x400;
pub const CDEV_OPEN: i32 = 0x400;
pub const CDEV_CLOSE: i32 = 0x401;
pub const CDEV_READ: i32 = 0x402;
pub const CDEV_WRITE: i32 = 0x403;
pub const CDEV_IOCTL: i32 = 0x404;
pub const CDEV_CANCEL: i32 = 0x405;
pub const CDEV_SELECT: i32 = 0x406;
