//! ProcFS per-process PID directory files — adapted from `minix/fs/procfs/pid.c`
//!
//! C lists `psinfo`, `cmdline`, `environ` and `map` here. Each needs something this port does not
//! have yet (`cmdline`/`environ` read the target's frame through VM, `map` reads VM's regions,
//! and `psinfo` is a binary struct joined from the kernel, PM, VFS and VM tables), so the list
//! carries the one file that can be produced truthfully today: a text `status`. That is a
//! deviation from C, recorded in `.notes/procfs.md`, and it is deliberately *not* C's formats
//! with invented numbers in them.

use crate::procfs::buf::buf_write_fmt;
use crate::procfs::consts::*;
use crate::procfs::tree;
use crate::procfs::types::{File, FileData};
use arch_common::types::{PM_PROC_STOPPED, PM_PROC_ZOMBIE, ProcTabEntry};

/// Files that appear in each per-process PID directory.
///
/// Terminated by an entry with `None` data.
pub static PID_FILES: &[File] = &[
    File {
        name: "status",
        mode: REG_ALL_MODE,
        data: FileData::Dynamic(pid_status),
    },
    // A symlink to the program this process is running, as `execve` was told it.
    // Its target is produced at read time by `tree::rdlink_hook`, which finds the
    // slot through this inode's parent; `FileData::None` because a link has no
    // content of its own.
    File {
        name: "exe",
        mode: LNK_ALL_MODE,
        data: FileData::None,
    },
    // Sentinel
    File {
        name: "",
        mode: 0,
        data: FileData::None,
    },
];

/// The state letter `/proc/<pid>/status` prints, from PM's flags.
///
/// C derives a richer set of letters from PM's flags *and* the kernel's run-state flags. PM's view
/// alone distinguishes a zombie and a stopped process; everything else PM has in use is reported
/// as `R`, which is the honest reading of "in use and not stopped or zombie".
pub fn state_letter(e: &ProcTabEntry) -> char {
    if e.flags & PM_PROC_ZOMBIE != 0 {
        'Z'
    } else if e.flags & PM_PROC_STOPPED != 0 {
        'T'
    } else {
        'R'
    }
}

/// Check whether the given slot is a zombie process.
pub fn is_zombie(slot: i32) -> bool {
    tree::entry(slot).flags & PM_PROC_ZOMBIE != 0
}

/// A text summary of one process.
///
/// C's `psinfo` is what `ps(1)` parses; this is the readable form that the record behind
/// `SI_PROC_TAB` can fill in today.
fn pid_status(slot: i32) {
    let e = tree::entry(slot);
    buf_write_fmt(format_args!("Name:   {}\n", e.name_str()));
    buf_write_fmt(format_args!("State:  {}\n", state_letter(&e)));
    buf_write_fmt(format_args!("Pid:    {}\n", e.pid));
    buf_write_fmt(format_args!("PPid:   {}\n", e.ppid));
    buf_write_fmt(format_args!("Uid:    {}\n", e.euid));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procfs::buf;

    #[test]
    fn state_letters_come_from_the_flags() {
        let mut e = ProcTabEntry::empty();
        e.flags = arch_common::types::PM_PROC_IN_USE;
        assert_eq!(state_letter(&e), 'R');
        e.flags |= PM_PROC_ZOMBIE;
        assert_eq!(state_letter(&e), 'Z');
        e.flags = arch_common::types::PM_PROC_IN_USE | PM_PROC_STOPPED;
        assert_eq!(state_letter(&e), 'T');
    }

    #[test]
    fn pid_files_has_sentinel_and_names_status() {
        assert_eq!(PID_FILES[0].name, "status");
        assert_eq!(PID_FILES[1].name, "exe");
        assert_eq!(PID_FILES[1].mode, LNK_ALL_MODE, "`exe` is a link, not a file");
        let last = PID_FILES.last().unwrap();
        assert!(matches!(last.data, crate::procfs::types::FileData::None));
    }

    #[test]
    fn status_of_an_absent_slot_is_empty_not_panic() {
        buf::buf_init(0, 128);
        pid_status(-1);
        let (_, len) = buf::buf_get();
        // A slot outside the table yields the empty record, whose fields are all zero.
        assert!(len > 0);
    }
}
