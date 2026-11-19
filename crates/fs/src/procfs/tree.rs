//! ProcFS VTreeFS hook implementations — adapted from `minix/fs/procfs/tree.c`
//!
//! The per-`pid` directories are built from PM's process table, asked for on demand
//! (`SI_PROC_TAB`, see `crate::procfs::tree::refresh_proc_tab`). A directory's inode carries its
//! process slot in `INode.index`, which is how the read hook and the entry constructor find the
//! process behind a node without a second table of their own.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

use arch_common::consts::NR_PROCS;
use arch_common::types::{PM_PROC_IN_USE, ProcTabEntry};

use crate::procfs::buf;
use crate::procfs::consts::*;
use crate::procfs::main::CB_DYNAMIC;
use crate::procfs::pid::PID_FILES;

/// `PM_GETSYSINFO` (`PM_BASE + 47`), and the `mess_lsys_getsysinfo` fields: C's union puts
/// `who`, `what`, `where` and `size` at these payload offsets.
#[cfg(target_os = "minix")]
const PM_GETSYSINFO: i32 = 47;
#[cfg(target_os = "minix")]
const GETSYSINFO_WHAT_OFF: usize = 12;
#[cfg(target_os = "minix")]
const GETSYSINFO_WHERE_OFF: usize = 16;
#[cfg(target_os = "minix")]
const GETSYSINFO_SIZE_OFF: usize = 24;
/// `getsysinfo(PM_PROC_NR, SI_PROC_TAB, ...)` (`minix/sysinfo.h`).
#[cfg(target_os = "minix")]
const SI_PROC_TAB: i32 = 2;

struct ProcTab(UnsafeCell<[ProcTabEntry; NR_PROCS]>);

// Safety: the buffer is written and read by this single-threaded server; the `Sync` impl is the
// same interior-mutability bargain `vtreefs` and the servers' own tables make.
unsafe impl Sync for ProcTab {}

impl ProcTab {
    const fn new() -> Self {
        Self(UnsafeCell::new([ProcTabEntry::empty(); NR_PROCS]))
    }
    fn get(&self) -> *mut [ProcTabEntry; NR_PROCS] {
        self.0.get()
    }
}

static PROC_TAB: ProcTab = ProcTab::new();
/// Whether `PROC_TAB` holds a table PM actually answered with.
static TAB_VALID: AtomicBool = AtomicBool::new(false);

/// Ask PM for its process table, into the static buffer. `false` when the call did not answer.
///
/// A host build has no PM and nothing reads `/proc` there, so it answers `false` and the tree
/// stays at its root files.
pub fn refresh_proc_tab() -> bool {
    #[cfg(target_os = "minix")]
    {
        let tab = unsafe { &mut *PROC_TAB.get() };
        let len = core::mem::size_of_val(tab);
        let mut msg = [0u8; 64];
        msg[4..8].copy_from_slice(&PM_GETSYSINFO.to_le_bytes());
        msg[GETSYSINFO_WHAT_OFF..GETSYSINFO_WHAT_OFF + 4]
            .copy_from_slice(&SI_PROC_TAB.to_le_bytes());
        msg[GETSYSINFO_WHERE_OFF..GETSYSINFO_WHERE_OFF + 8]
            .copy_from_slice(&(tab.as_ptr() as u64).to_ne_bytes());
        msg[GETSYSINFO_SIZE_OFF..GETSYSINFO_SIZE_OFF + 8]
            .copy_from_slice(&(len as u64).to_ne_bytes());
        if minix_rt::sendrec(arch_common::com::PM_PROC_NR, &mut msg) != OK {
            TAB_VALID.store(false, Ordering::Relaxed);
            return false;
        }
        TAB_VALID.store(true, Ordering::Relaxed);
        true
    }
    #[cfg(not(target_os = "minix"))]
    {
        TAB_VALID.store(false, Ordering::Relaxed);
        false
    }
}

/// The record for one process slot, or an empty one when the table has never been fetched.
pub fn entry(slot: i32) -> ProcTabEntry {
    if !TAB_VALID.load(Ordering::Relaxed) {
        return ProcTabEntry::empty();
    }
    let i = slot;
    if i < 0 || i as usize >= NR_PROCS {
        return ProcTabEntry::empty();
    }
    unsafe { (*PROC_TAB.get())[i as usize] }
}

/// Is this slot a live process PM knows about?
pub fn slot_in_use(slot: i32) -> bool {
    let e = entry(slot);
    e.flags & PM_PROC_IN_USE != 0 && e.pid != 0
}

/// Place a table straight into the static buffer, for the host tests.
#[cfg(test)]
pub fn set_table(entries: &[ProcTabEntry]) {
    let tab = unsafe { &mut *PROC_TAB.get() };
    *tab = [ProcTabEntry::empty(); NR_PROCS];
    for (i, e) in entries.iter().enumerate().take(NR_PROCS) {
        tab[i] = *e;
    }
    TAB_VALID.store(true, Ordering::Relaxed);
}

/// The inode of the directory named after `pid`, if it exists under `parent`.
fn find_pid_dir(parent: u32, pid: i32) -> Option<u32> {
    let mut name = [0u8; 12];
    let s = format_pid(&mut name, pid);
    libs::vtreefs::find_inode(parent, s)
}

/// Write `pid` into `buf` as decimal, returning the string slice.
///
/// A small local itoa: `core::fmt` into a stack buffer would do the same job, but this keeps the
/// hook free of formatting machinery on a path that runs per directory entry.
fn format_pid(buf: &mut [u8; 12], pid: i32) -> &str {
    let mut v = pid.max(0) as u32;
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    core::str::from_utf8(&buf[i..]).unwrap_or("?")
}

/// Regenerate the set of PID directories in the root.
///
/// C's `tree.c` does the same on `getdents` of the root, so a process that starts after the mount
/// still appears. An entry that is already there is left alone: adding it twice would give the
/// root two children with one name.
pub fn construct_pid_dirs() {
    let root = libs::vtreefs::get_root_inode();
    for slot in 0..NR_PROCS as i32 {
        if !slot_in_use(slot) {
            continue;
        }
        let pid = entry(slot).pid;
        if find_pid_dir(root, pid).is_some() {
            continue;
        }
        let mut name_buf = [0u8; 12];
        let name = format_pid(&mut name_buf, pid);
        let stat = libs::vtreefs::InodeStat {
            mode: DIR_ALL_MODE,
            uid: SUPER_USER as u32,
            gid: SUPER_USER as u32,
            size: 0,
            dev: NO_DEV as u64,
        };
        // `index` is the process slot; the read hook reads it back from the parent.
        libs::vtreefs::add_inode(root, name, slot, &stat, 0);
    }
}

/// Construct the files of one PID directory.
///
/// The files are indexed children, as in C's `make_all_pid_entries`: `getdents` walks a
/// directory's indexed range by index, so a PID's files are listed from it rather than as
/// ordinary children.
pub fn construct_pid_entries(parent: u32) {
    let slot = libs::vtreefs::get_inode(parent).index;
    if slot < 0 {
        return;
    }
    crate::procfs::main::construct_indexed_tree(parent as u16, PID_FILES);
}

/// Path name resolution hook.
///
/// Tries `find_inode` first. When that misses, the directory's entries are built: the root's
/// process directories, or a PID directory's files.
pub fn lookup_hook(parent: u32, name: &str) -> i32 {
    if libs::vtreefs::find_inode(parent, name).is_some() {
        return OK;
    }

    let index = libs::vtreefs::get_inode(parent).index;
    if index < 0 {
        if refresh_proc_tab() {
            construct_pid_dirs();
        }
    } else {
        construct_pid_entries(parent);
    }

    if libs::vtreefs::find_inode(parent, name).is_some() {
        OK
    } else {
        EINVAL
    }
}

/// Directory entry retrieval hook.
///
/// Refreshes the table so a process that started after the mount is visible, then regenerates
/// the entries of this directory.
pub fn getdents_hook(node: u32) -> i32 {
    let index = libs::vtreefs::get_inode(node).index;
    if index < 0 {
        if refresh_proc_tab() {
            construct_pid_dirs();
        }
    } else {
        construct_pid_entries(node);
    }
    OK
}

/// Regular file read hook.
///
/// Initialises the output buffer, decodes `cbdata` to find the file handler, calls it, copies the
/// produced bytes into `buf`, and returns the byte count.
pub fn read_hook(node: u32, offset: u64, buf: &mut [u8]) -> usize {
    buf::buf_init(offset, buf.len());

    let cbdata = libs::vtreefs::get_inode_cbdata(node);
    if cbdata != 0 {
        if cbdata & CB_DYNAMIC != 0 {
            // Dynamic handler: fn(i32), with the process slot in the parent's index.
            let parent = libs::vtreefs::get_inode(node).parent_id;
            let slot = libs::vtreefs::get_inode(parent).index;
            let f: fn(i32) = unsafe { core::mem::transmute((cbdata & !CB_DYNAMIC) as *const ()) };
            f(slot);
        } else {
            let f: fn() = unsafe { core::mem::transmute(cbdata as *const ()) };
            f();
        }
    }

    let (data, len) = buf::buf_get();
    let n = len.min(buf.len());
    buf[..n].copy_from_slice(&data[..n]);
    n
}

/// Symbolic link resolution hook.
pub fn rdlink_hook(_node: u32, _ptr: &mut [u8]) -> i32 {
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use arch_common::types::{PM_PROC_ZOMBIE, PROC_NAME_LEN};

    fn name_of(s: &str) -> [u8; PROC_NAME_LEN] {
        let mut n = [0u8; PROC_NAME_LEN];
        for (i, &b) in s.as_bytes().iter().enumerate().take(PROC_NAME_LEN - 1) {
            n[i] = b;
        }
        n
    }

    fn live(pid: i32, name: &str) -> ProcTabEntry {
        ProcTabEntry {
            pid,
            ppid: 1,
            endpoint: 0x10000 | pid,
            euid: 0,
            flags: PM_PROC_IN_USE,
            name: name_of(name),
        }
    }

    /// Every test builds its own tree; `vtreefs_init` resets the table, so they cannot share one.
    fn init() {
        crate::procfs::main::init_tree();
    }

    #[test]
    fn format_pid_writes_decimal() {
        let mut b = [0u8; 12];
        assert_eq!(format_pid(&mut b, 0), "0");
        let mut b = [0u8; 12];
        assert_eq!(format_pid(&mut b, 42), "42");
        let mut b = [0u8; 12];
        assert_eq!(format_pid(&mut b, 1234), "1234");
    }

    #[test]
    fn pid_dirs_appear_for_live_processes_only() {
        init();
        let mut dead = live(99, "dead");
        dead.flags = 0;
        set_table(&[live(42, "init"), dead]);

        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).expect("a live process gets a directory");
        assert!(find_pid_dir(root, 99).is_none(), "an empty slot does not");

        // The slot travels in the inode's index, which is what the read hook reads back.
        assert_eq!(libs::vtreefs::get_inode(dir).index, 0);
    }

    #[test]
    fn pid_dirs_are_indexed_children_of_the_root() {
        // `getdents` walks a directory's indexed range by index. A PID directory that was not
        // indexed would be skipped by that walk and never listed.
        init();
        set_table(&[live(42, "init"), live(43, "shell")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let first = libs::vtreefs::get_inode_by_index(root, 0).expect("slot 0 is indexed");
        assert_eq!(libs::vtreefs::get_inode_name(first), "42");
        let second = libs::vtreefs::get_inode_by_index(root, 1).expect("slot 1 is indexed");
        assert_eq!(libs::vtreefs::get_inode_name(second), "43");
        assert_eq!(libs::vtreefs::get_inode(root).indexed, 2);
        assert!(libs::vtreefs::get_inode_by_index(root, 2).is_none());
    }

    fn child_count(parent: u32, name: &str) -> usize {
        let mut n = 0;
        let mut cur = libs::vtreefs::first_inode(parent);
        while let Some(c) = cur {
            if libs::vtreefs::get_inode_name(c) == name {
                n += 1;
            }
            cur = libs::vtreefs::next_sibling(c);
        }
        n
    }

    #[test]
    fn a_pid_dir_adds_its_files_once() {
        // The hook runs on every read of the directory, so a second call must not add a
        // second `status`.
        init();
        set_table(&[live(42, "init")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).unwrap();

        construct_pid_entries(dir);
        construct_pid_entries(dir);

        assert_eq!(child_count(dir, "status"), 1);
        let status = libs::vtreefs::get_inode_by_index(dir, 0).expect("status is indexed");
        assert_eq!(libs::vtreefs::get_inode_name(status), "status");
        assert_eq!(libs::vtreefs::get_inode(dir).indexed, 1);
    }

    #[test]
    fn a_pid_dir_gains_its_status_file() {
        init();
        set_table(&[live(42, "init")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).unwrap();

        assert_eq!(lookup_hook(dir, "status"), OK);
        assert!(libs::vtreefs::find_inode(dir, "status").is_some());
        assert_eq!(lookup_hook(dir, "nope"), EINVAL);
    }

    #[test]
    fn status_reads_the_process_it_belongs_to() {
        init();
        let mut z = live(77, "zombie");
        z.flags = PM_PROC_IN_USE | PM_PROC_ZOMBIE;
        z.ppid = 3;
        z.euid = 1000;
        set_table(&[live(42, "init"), z]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 77).unwrap();
        construct_pid_entries(dir);
        let file = libs::vtreefs::find_inode(dir, "status").expect("status was constructed");

        let mut out = [0u8; 256];
        let n = read_hook(file, 0, &mut out);
        let text = core::str::from_utf8(&out[..n]).unwrap();
        assert!(text.contains("Name:   zombie"), "{text}");
        assert!(text.contains("State:  Z"), "{text}");
        assert!(text.contains("Pid:    77"), "{text}");
        assert!(text.contains("PPid:   3"), "{text}");
        assert!(text.contains("Uid:    1000"), "{text}");
    }

    #[test]
    fn a_stopped_process_reads_as_t() {
        init();
        let mut s = live(5, "stopped");
        s.flags = PM_PROC_IN_USE | arch_common::types::PM_PROC_STOPPED;
        set_table(&[s]);
        assert_eq!(crate::procfs::pid::state_letter(&entry(0)), 'T');
        assert_eq!(crate::procfs::pid::state_letter(&live(1, "run")), 'R');
    }

    #[test]
    fn reading_without_a_table_finds_no_directories() {
        init();
        // No `set_table`, and the host has no PM: lookups under the root fail rather than panic.
        assert_eq!(lookup_hook(libs::vtreefs::get_root_inode(), "42"), EINVAL);
    }
}
