//! ProcFS per-process PID directory files — adapted from `minix/fs/procfs/pid.c`

use crate::procfs::buf::buf_write;
use crate::procfs::consts::*;
use crate::procfs::types::{File, FileData};

/// Files that appear in each per-process PID directory.
///
/// Terminated by an entry with `None` data.
pub static PID_FILES: &[File] = &[
    File {
        name: "psinfo",
        mode: REG_ALL_MODE,
        data: FileData::Dynamic(pid_psinfo),
    },
    File {
        name: "cmdline",
        mode: REG_ALL_MODE,
        data: FileData::Dynamic(pid_cmdline),
    },
    File {
        name: "environ",
        mode: REG_ALL_MODE,
        data: FileData::Dynamic(pid_environ),
    },
    File {
        name: "map",
        mode: REG_ALL_MODE,
        data: FileData::Dynamic(pid_map),
    },
    // Sentinel
    File {
        name: "",
        mode: 0,
        data: FileData::None,
    },
];

/// Check whether the given slot is a zombie process.
///
/// TODO: check `mproc[slot - NR_TASKS].mp_flags & (TRACE_ZOMBIE | ZOMBIE)`.
pub fn is_zombie(_slot: i32) -> bool {
    false
}

/// Print information used by `ps(1)` and `top(1)`.
///
/// TODO: read from `proc[]`, `mproc[]`, `fproc[]` tables and format
///       `PSINFO_VERSION` line plus extended info.
fn pid_psinfo(_slot: i32) {
    buf_write("1 T 0 (stub) R 0 0 0 0 0\n");
}

/// Dump the process's command line.
///
/// TODO: call `get_frame()` and use `sys_datacopy()` to read from the
///       target process, then `buf_append()` each null-terminated arg.
fn pid_cmdline(_slot: i32) {
    // Nothing written (stub).
}

/// Dump the process's environment.
///
/// TODO: similar to `pid_cmdline()` but reads environment pointers.
fn pid_environ(_slot: i32) {
    // Nothing written (stub).
}

/// Print a memory map of the process.
///
/// TODO: call `vm_info_region()` to get regions, format as
///       `"%08lx-%08lx %c%c%c\n"` lines.
fn pid_map(_slot: i32) {
    // Nothing written (stub).
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::buf;

    #[test]
    fn pid_psinfo_does_not_panic() {
        buf::buf_init(0, 128);
        pid_psinfo(0);
        let (_, len) = buf::buf_get();
        assert!(len > 0);
    }

    #[test]
    fn is_zombie_returns_false() {
        assert!(!is_zombie(0));
    }

    #[test]
    fn pid_files_has_sentinel() {
        let last = PID_FILES.last().unwrap();
        assert!(matches!(last.data, crate::procfs::types::FileData::None));
    }
}
