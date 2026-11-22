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
///
/// Used by the tests and by a caller that already has a pid; `construct_pid_dirs` looks
/// directories up by slot instead, because a reused slot's old directory carries the wrong
/// name and must not be found by it.
#[cfg(test)]
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
/// C's `tree.c` does the same on `getdents` of the root, so a process that starts after the
/// mount still appears. It runs in two passes, as C does (`tree.c:213`): a directory whose
/// slot is free, or whose name is no longer the pid that slot holds, describes a process
/// that is gone, and it is deleted before anything is added. A slot reused by a new pid
/// would otherwise briefly have two directories with one index, and the name guard would
/// hand the new process the old directory, whose index is the *old* slot.
pub fn construct_pid_dirs() {
    let root = libs::vtreefs::get_root_inode();

    // First pass: delete stale directories.
    for slot in 0..NR_PROCS as i32 {
        let Some(node) = libs::vtreefs::get_inode_by_index(root, slot) else {
            continue;
        };
        if !slot_in_use(slot) {
            libs::vtreefs::delete_inode(node);
            continue;
        }
        let mut name_buf = [0u8; 12];
        let want = format_pid(&mut name_buf, entry(slot).pid);
        // The owner is not compared: every PID directory in this port is created with
        // SUPER_USER, so there is nothing an owner change could alter (C reads PM's euid
        // here, `tree.c:261`).
        if libs::vtreefs::get_inode_name(node) != want {
            libs::vtreefs::delete_inode(node);
        }
    }

    // Second pass: add what is missing.
    for slot in 0..NR_PROCS as i32 {
        if !slot_in_use(slot) {
            continue;
        }
        if libs::vtreefs::get_inode_by_index(root, slot).is_some() {
            continue;
        }
        let pid = entry(slot).pid;
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
///
/// Two links in this tree depend on something the path cannot carry:
///
/// * `/proc/<pid>/exe` names the program the process was started from. PM records
///   the path `execve` was given and hands it over with the process table.
/// * `/proc/self` means "the caller's own pid directory", so its target is built
///   from `caller`, the endpoint VFS passes with the lookup or the readlink.
///
/// Anything else is not a link this filesystem serves. A link whose data is
/// missing (a boot process has no recorded path; a lookup with no caller) answers
/// `ENOENT`, so a reader gets "no such file" rather than a link to nowhere.
pub fn rdlink_hook(node: u32, caller: i32, ptr: &mut [u8]) -> i32 {
    // A path through `/proc/self` can reach this hook without any lookup that
    // would have refreshed the table: `self` is a static root entry, so its
    // lookup is a hit. Both targets read from the table (the caller's slot, and
    // the process's exec path), so ask PM first. A host build has no PM; its
    // tests install the table directly.
    #[cfg(target_os = "minix")]
    refresh_proc_tab();

    let inode = libs::vtreefs::get_inode(node);
    let name = inode_name(inode);

    if name == b"exe" {
        let slot = libs::vtreefs::get_inode(inode.parent_id).index;
        let e = entry(slot);
        let path = e.exec_str();
        if path.is_empty() {
            return ENOENT;
        }
        return write_target(ptr, path.as_bytes());
    }

    if name == b"self" {
        let Some(slot) = slot_by_endpoint(caller) else {
            return ENOENT;
        };
        let mut digits_buf = [0u8; 12];
        let digits = format_pid(&mut digits_buf, entry(slot).pid).as_bytes();
        let mut out = [0u8; 18];
        out[..6].copy_from_slice(b"/proc/");
        let n = 6 + digits.len();
        out[6..n].copy_from_slice(digits);
        return write_target(ptr, &out[..n]);
    }

    EINVAL
}

/// Copy a link target into the hook's buffer, NUL-terminated.
fn write_target(ptr: &mut [u8], target: &[u8]) -> i32 {
    if target.len() + 1 > ptr.len() {
        return ENAMETOOLONG;
    }
    ptr[..target.len()].copy_from_slice(target);
    ptr[target.len()] = 0;
    OK
}

/// An inode's name, stopping at the first NUL.
fn inode_name(inode: &libs::vtreefs::INode) -> &[u8] {
    let end = inode.name.iter().position(|&b| b == 0).unwrap_or(inode.name.len());
    &inode.name[..end]
}

/// The process slot whose endpoint is `endpoint`, if PM has one.
///
/// `/proc/self` is resolved this way rather than from a pid carried in the
/// request: the endpoint is what both VFS's fproc table and PM's `mp_endpoint`
/// speak, and the pid is a property of the slot.
fn slot_by_endpoint(endpoint: i32) -> Option<i32> {
    if endpoint == 0 {
        return None;
    }
    for slot in 0..NR_PROCS as i32 {
        let e = entry(slot);
        if e.endpoint == endpoint && e.flags & PM_PROC_IN_USE != 0 {
            return Some(slot);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use arch_common::types::{PM_PROC_ZOMBIE, PROC_EXEC_LEN, PROC_NAME_LEN};

    fn name_of(s: &str) -> [u8; PROC_NAME_LEN] {
        let mut n = [0u8; PROC_NAME_LEN];
        for (i, &b) in s.as_bytes().iter().enumerate().take(PROC_NAME_LEN - 1) {
            n[i] = b;
        }
        n
    }

    fn live(pid: i32, name: &str) -> ProcTabEntry {
        live_with_exec(pid, name, "")
    }

    fn live_with_exec(pid: i32, name: &str, exec: &str) -> ProcTabEntry {
        let mut path = [0u8; PROC_EXEC_LEN];
        for (i, &b) in exec.as_bytes().iter().enumerate().take(PROC_EXEC_LEN - 1) {
            path[i] = b;
        }
        ProcTabEntry {
            pid,
            ppid: 1,
            endpoint: 0x10000 | pid,
            euid: 0,
            flags: PM_PROC_IN_USE,
            name: name_of(name),
            exec: path,
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
        // second `status` or a second `exe`.
        init();
        set_table(&[live(42, "init")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).unwrap();

        construct_pid_entries(dir);
        construct_pid_entries(dir);

        assert_eq!(child_count(dir, "status"), 1);
        assert_eq!(child_count(dir, "exe"), 1);
        let status = libs::vtreefs::get_inode_by_index(dir, 0).expect("status is indexed");
        assert_eq!(libs::vtreefs::get_inode_name(status), "status");
        let exe = libs::vtreefs::get_inode_by_index(dir, 1).expect("exe is indexed");
        assert_eq!(libs::vtreefs::get_inode_name(exe), "exe");
        assert_eq!(libs::vtreefs::get_inode(dir).indexed, 2);
    }

    #[test]
    fn a_freed_slot_loses_its_directory() {
        init();
        set_table(&[live(42, "init")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        assert!(find_pid_dir(root, 42).is_some());

        // The process exits: the slot is free on the next read of the root.
        set_table(&[]);
        construct_pid_dirs();
        assert!(find_pid_dir(root, 42).is_none(), "a dead slot's directory goes");
        assert!(libs::vtreefs::get_inode_by_index(root, 0).is_none());
    }

    #[test]
    fn a_reused_slot_rebuilds_its_directory() {
        // A slot given to a different pid must not keep the old directory: it carries the
        // old slot in its index, and `construct_pid_entries` reads that index as the slot,
        // so `status` would describe the previous process.
        init();
        set_table(&[live(42, "init")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        assert!(find_pid_dir(root, 42).is_some());

        set_table(&[live(77, "shell")]);
        construct_pid_dirs();
        assert!(find_pid_dir(root, 42).is_none(), "the old name goes away");
        let new = find_pid_dir(root, 77).expect("the new pid gets its own directory");
        assert_eq!(libs::vtreefs::get_inode(new).index, 0);

        construct_pid_entries(new);
        let status = libs::vtreefs::find_inode(new, "status").expect("status exists");
        let mut out = [0u8; 256];
        let n = read_hook(status, 0, &mut out);
        let text = core::str::from_utf8(&out[..n]).unwrap();
        assert!(text.contains("Pid:    77"), "status names the new process: {text}");
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

    /// Build the root's static entries, which `init_tree` leaves to the mount's `init_hook`.
    fn construct_root_files() {
        let root = libs::vtreefs::get_root_inode() as u16;
        crate::procfs::main::construct_tree(root, crate::procfs::root::ROOT_FILES);
    }

    /// A NUL-terminated link target, as `write_target` leaves it.
    fn target_of(buf: &[u8]) -> &[u8] {
        let end = buf.iter().position(|&b| b == 0).expect("the target is NUL-terminated");
        &buf[..end]
    }

    #[test]
    fn exe_link_names_the_program_the_process_ran() {
        init();
        set_table(&[live_with_exec(42, "clang", "/bin/coreutils")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).unwrap();
        construct_pid_entries(dir);
        let exe = libs::vtreefs::find_inode(dir, "exe").expect("exe is constructed");

        let mut out = [0u8; 64];
        assert_eq!(rdlink_hook(exe, 0, &mut out), OK);
        assert_eq!(target_of(&out), b"/bin/coreutils");
    }

    #[test]
    fn exe_link_without_a_recorded_path_is_enoent() {
        // A boot process was never `execve`d, so PM has no path to hand over. A reader gets
        // "no such file", not a link to nowhere.
        init();
        set_table(&[live(42, "boot")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).unwrap();
        construct_pid_entries(dir);
        let exe = libs::vtreefs::find_inode(dir, "exe").unwrap();

        let mut out = [0u8; 64];
        assert_eq!(rdlink_hook(exe, 0, &mut out), ENOENT);
    }

    #[test]
    fn self_link_names_the_callers_directory() {
        // The target depends on who asks, so it comes from the caller's endpoint rather than
        // from the inode: the same `self` inode answers differently for two processes.
        init();
        construct_root_files();
        set_table(&[live(42, "init"), live(43, "coreutils")]);
        let root = libs::vtreefs::get_root_inode();
        let link = libs::vtreefs::find_inode(root, "self").expect("self is a root entry");

        let mut out = [0u8; 32];
        assert_eq!(rdlink_hook(link, 0x10000 | 43, &mut out), OK);
        assert_eq!(target_of(&out), b"/proc/43");

        let mut out = [0u8; 32];
        assert_eq!(rdlink_hook(link, 0x10000 | 42, &mut out), OK);
        assert_eq!(target_of(&out), b"/proc/42");
    }

    #[test]
    fn self_link_without_a_known_caller_is_enoent() {
        init();
        construct_root_files();
        set_table(&[live(42, "init")]);
        let root = libs::vtreefs::get_root_inode();
        let link = libs::vtreefs::find_inode(root, "self").unwrap();

        let mut out = [0u8; 32];
        // No caller at all, and an endpoint that belongs to nobody.
        assert_eq!(rdlink_hook(link, 0, &mut out), ENOENT);
        assert_eq!(rdlink_hook(link, 0x9999, &mut out), ENOENT);
    }

    #[test]
    fn a_regular_file_is_not_a_link() {
        init();
        set_table(&[live(42, "init")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).unwrap();
        construct_pid_entries(dir);
        let status = libs::vtreefs::find_inode(dir, "status").unwrap();

        let mut out = [0u8; 64];
        assert_eq!(rdlink_hook(status, 0, &mut out), EINVAL);
    }

    #[test]
    fn a_target_too_long_for_the_buffer_names_the_error() {
        init();
        set_table(&[live_with_exec(42, "clang", "/bin/coreutils")]);
        construct_pid_dirs();
        let root = libs::vtreefs::get_root_inode();
        let dir = find_pid_dir(root, 42).unwrap();
        construct_pid_entries(dir);
        let exe = libs::vtreefs::find_inode(dir, "exe").unwrap();

        // "/bin/coreutils" plus its NUL is 15 bytes; 14 leaves no room for the terminator.
        let mut out = [0u8; 14];
        assert_eq!(rdlink_hook(exe, 0, &mut out), ENAMETOOLONG);
    }
}
