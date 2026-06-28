//! ProcFS constants — adapted from `minix/fs/procfs/const.h`

// Placeholder constants; these will come from the kernel config in a real build.
pub const NR_TASKS: usize = 4;
pub const NR_PROCS: usize = 32;

/// Number of inodes: enough for statically created files plus all PID directories.
pub const NR_INODES: usize = (NR_TASKS + NR_PROCS) * 4;

// ── File type mode bits (from Minix `<sys/stat.h>`) ──
pub const S_IFMT: u32 = 0o170000; /* type mask */
pub const S_IFREG: u32 = 0o100000; /* regular file */
pub const S_IFDIR: u32 = 0o040000; /* directory */
pub const S_IFLNK: u32 = 0o120000; /* symbolic link */

/// World-readable regular file.
pub const REG_ALL_MODE: u32 = S_IFREG | 0o444;
/// World-accessible directory.
pub const DIR_ALL_MODE: u32 = S_IFDIR | 0o555;
/// Symbolic link (world-all).
pub const LNK_ALL_MODE: u32 = S_IFLNK | 0o777;

/// No-device sentinel.
pub const NO_DEV: u32 = 0xFFFF;
/// Super-user UID/GID.
pub const SUPER_USER: u16 = 0;

/// Maximum process-file name length.
pub const PNAME_MAX: usize = 255;

/// PSINFO format version.
pub const PSINFO_VERSION: i32 = 1;

/// Index sentinel meaning "no index" (for static files).
pub const NO_INDEX: i32 = -1;

// ── Errno / OK ──
pub const OK: i32 = 0;
pub const EINVAL: i32 = 22;

// ── State / type constants for psinfo file ──
pub const TYPE_TASK: char = 'T';
pub const TYPE_SYSTEM: char = 'S';
pub const TYPE_USER: char = 'U';
pub const STATE_RUN: char = 'R';
pub const STATE_SLEEP: char = 'S';
pub const STATE_WAIT: char = 'W';
pub const STATE_ZOMBIE: char = 'Z';
pub const STATE_STOP: char = 'T';
pub const PSTATE_WAITING: char = 'W';
pub const PSTATE_SIGSUSP: char = 'S';
pub const FSTATE_NONE: char = '-';
pub const FSTATE_PIPE: char = 'P';
pub const FSTATE_LOCK: char = 'L';
pub const FSTATE_POPEN: char = 'p';
pub const FSTATE_SELECT: char = 's';
pub const FSTATE_TASK: char = 'T';
pub const FSTATE_UNKNOWN: char = '?';

// ── VTreeFS stub constants ──
pub const NO_PID: i32 = -1;
pub const NONE: i32 = -1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_sane() {
        assert_eq!(SUPER_USER, 0);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn constant_expressions_are_valid() {
        const _: () = assert!(NR_INODES > 0);
        const _: () = assert!(REG_ALL_MODE & S_IFREG != 0);
        const _: () = assert!(DIR_ALL_MODE & S_IFDIR != 0);
        const _: () = assert!(LNK_ALL_MODE & S_IFLNK != 0);
    }
}
