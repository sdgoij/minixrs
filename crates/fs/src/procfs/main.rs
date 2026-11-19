//! ProcFS entry point — adapted from `minix/fs/procfs/main.c`

use crate::procfs::consts::*;
use crate::procfs::types::{File, FileData};

// Re-export the VTreeFS constants/functions we use.
use libs::vtreefs;

/// Convenience: encode a `FileData` value into a `vtreefs::CbData`.
///
/// The inode's callback data is one `usize`, so the kind of handler has to share it with the
/// function pointer:
/// - `FileData::None`      -> 0
/// - `FileData::Static(f)` -> `f as usize`
/// - `FileData::Dynamic(f)`-> `(f as usize) | CB_DYNAMIC`
fn file_data_to_cbdata(data: &FileData) -> vtreefs::CbData {
    match data {
        FileData::None => 0,
        FileData::Static(f) => *f as usize,
        FileData::Dynamic(f) => (*f as usize) | CB_DYNAMIC,
    }
}

/// Tag that marks a `FileData::Dynamic` handler in an inode's callback data.
///
/// Bit 0 cannot carry the tag. A function pointer is aligned on the three hardware arches but
/// not on wasm32, where it is an index into the indirect-function table: `root_hz` can land on
/// an odd index, and OR-ing bit 0 then either clears it to null or aliases it onto a
/// neighbouring function. That was a real trap in the wasm system on `cat /proc/hz`
/// (`RuntimeError: null function or function signature mismatch`).
///
/// The top bit is free on all four targets: the hardware arches keep userland code well inside
/// the low canonical range, and a wasm table index is small.
pub(crate) const CB_DYNAMIC: usize = 1 << (usize::BITS - 1);

/// Add an inode to the virtual tree.
fn add_inode(parent: u16, name: &str, index: i32, mode: u32, data: &FileData) -> u16 {
    let stat = vtreefs::InodeStat {
        mode,
        uid: SUPER_USER as u32,
        gid: SUPER_USER as u32,
        size: 0,
        dev: NO_DEV as u64,
    };
    let cbdata = file_data_to_cbdata(data);
    vtreefs::add_inode(parent as u32, name, index, &stat, cbdata) as u16
}

/// Return the root inode index (always 0).
fn get_root_inode() -> u16 {
    vtreefs::get_root_inode() as u16
}

/// Initialization hook called by VTreeFS at startup.
///
/// Constructs the static portion of the ProcFS tree. A directory's children are built by calling
/// [`construct_tree`] with that directory's own list, so per-`pid` directories slot in as one more
/// call when their file list exists (see `tree.rs`: the process tables behind them are stubs).
fn init_hook() {
    let root = get_root_inode();
    construct_tree(root, crate::procfs::root::ROOT_FILES);
}

/// Construct the entries of one directory from a sentinel-terminated `files` list.
///
/// The list is flat: C's `tree.c` recurses into a directory's own `files` array, and this port
/// passes each directory's array in its own call instead (nothing nests yet).
pub fn construct_tree(dir: u16, files: &[File]) {
    for file in files {
        // Sentinel check.
        if file.name.is_empty() {
            break;
        }

        let _node = add_inode(dir, file.name, NO_INDEX, file.mode, &file.data);
    }
}

/// Construct a directory's files as indexed children, once each.
///
/// This is C's `make_all_pid_entries`: the index is the file's position in `files`, which is
/// what `getdents` enumerates, and a file that is already present is left alone because the
/// hook runs on every read of the directory. Adding it again would list the same name twice.
pub fn construct_indexed_tree(dir: u16, files: &[File]) {
    for (i, file) in files.iter().enumerate() {
        if file.name.is_empty() {
            break;
        }
        if vtreefs::get_inode_by_index(dir as u32, i as i32).is_some() {
            continue;
        }
        let _node = add_inode(dir, file.name, i as i32, file.mode, &file.data);
    }
}

/// Build the VTreeFS hook table and root inode stat.
fn hooks_and_root_stat() -> (vtreefs::FsHooks, vtreefs::InodeStat) {
    let hooks = vtreefs::FsHooks {
        init_hook: Some(init_hook),
        cleanup_hook: None,
        lookup_hook: Some(crate::procfs::tree::lookup_hook),
        getdents_hook: Some(crate::procfs::tree::getdents_hook),
        read_hook: Some(crate::procfs::tree::read_hook),
        rdlink_hook: Some(crate::procfs::tree::rdlink_hook),
        message_hook: None,
    };

    let root_stat = vtreefs::InodeStat {
        mode: DIR_ALL_MODE,
        uid: SUPER_USER as u32,
        gid: SUPER_USER as u32,
        size: 0,
        dev: NO_DEV as u64,
    };
    (hooks, root_stat)
}

/// Initialize the ProcFS tree state.
///
/// This updates the process tables and counts PID directory entries.
/// Returns `OK` on success.
pub fn init_tree() -> i32 {
    let (hooks, root_stat) = hooks_and_root_stat();
    vtreefs::vtreefs_init(hooks, NR_INODES as u32, root_stat);
    OK
}

/// ProcFS entry point.
///
/// Initializes the tree, sets up root directory properties, and starts
/// the VTreeFS event loop (which does not return).
pub fn procfs_main() {
    let (hooks, root_stat) = hooks_and_root_stat();
    vtreefs::start_vtreefs(hooks, NR_INODES as u32, root_stat, 0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_handlers_are_tagged_with_the_top_bit() {
        // The kind of handler shares the inode's `cbdata` with its function pointer. Bit 0 is
        // not free on wasm32 (a function pointer is an indirect-table index there), so the tag
        // is the top bit and the pointer must survive masking.
        fn static_handler() {}
        fn dynamic_handler(_slot: i32) {}
        let s = file_data_to_cbdata(&FileData::Static(static_handler));
        let d = file_data_to_cbdata(&FileData::Dynamic(dynamic_handler));
        assert_eq!(s & CB_DYNAMIC, 0, "a static handler is untagged");
        assert_eq!(d & CB_DYNAMIC, CB_DYNAMIC, "a dynamic handler is tagged");
        assert_eq!(s, static_handler as *const () as usize, "the pointer is stored whole");
        assert_eq!(d & !CB_DYNAMIC, dynamic_handler as *const () as usize);
        assert_eq!(file_data_to_cbdata(&FileData::None), 0);
    }

    #[test]
    fn init_tree_returns_ok() {
        assert_eq!(init_tree(), OK);
    }

    #[test]
    fn init_hook_no_panic() {
        init_tree(); // initialises VTreeFS (init_hook fires on mount)
        init_hook(); // direct call should also be fine
    }

    #[test]
    fn construct_tree_empty() {
        init_tree();
        construct_tree(0, &[]);
    }

    #[test]
    fn construct_tree_with_sentinel() {
        init_tree();
        let files = [File {
            name: "",
            mode: 0,
            data: crate::procfs::types::FileData::None,
        }];
        construct_tree(0, &files);
    }
}
