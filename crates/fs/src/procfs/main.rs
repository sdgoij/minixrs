//! ProcFS entry point — adapted from `minix/fs/procfs/main.c`

use crate::procfs::consts::*;
use crate::procfs::types::{File, FileData};

// ── VTreeFS wrapper ────────────────────────────────────────────────────

// Re-export the VTreeFS constants/functions we use.
use libs::vtreefs;

/// Convenience: encode a `FileData` value into a `vtreefs::CbData`.
///
/// Encoding scheme:
/// - `FileData::None`      → 0
/// - `FileData::Static(f)` → `f as usize`        (bit 0 = 0 — function pointers are aligned)
/// - `FileData::Dynamic(f)`→ `(f as usize) | 1`  (bit 0 = 1 tag)
fn file_data_to_cbdata(data: &FileData) -> vtreefs::CbData {
    match data {
        FileData::None => 0,
        FileData::Static(f) => *f as usize,
        FileData::Dynamic(f) => (*f as usize) | 1,
    }
}

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

/// Start the VTreeFS event loop (does not return).
fn start_vtreefs() -> ! {
    vtreefs::start_vtreefs()
}

// ── Hook table ────────────────────────────────────────────────────────

/// Initialization hook called by VTreeFS at startup.
///
/// Constructs the static portion of the ProcFS tree.
#[allow(dead_code)]
fn init_hook() {
    let root = get_root_inode();
    construct_tree(root, crate::procfs::root::ROOT_FILES);
}

/// Recursively construct the static portion of the ProcFS tree.
///
/// For each non-sentinel entry in `files`:
/// - If the file mode has `S_IFDIR` set, the entry describes a subdirectory
///   to be created recursively.
/// - Otherwise, a regular inode is created.
pub fn construct_tree(dir: u16, files: &[File]) {
    for file in files {
        // Sentinel check.
        if file.name.is_empty() {
            break;
        }

        let _node = add_inode(dir, file.name, NO_INDEX, file.mode, &file.data);

        if file.mode & S_IFDIR != 0 {
            // Directory entry: recurse.
            // The child file list must be stored inside FileData — currently
            // FileData does not have a directory variant.  This will need to
            // be added when subdirectories (e.g. /proc/net/) are implemented.
            // For now, skip recursion.
        }
    }
}

/// Initialize the ProcFS tree state.
///
/// This updates the process tables and counts PID directory entries.
/// Returns `OK` on success.
pub fn init_tree() -> i32 {
    // Build the hook table.
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

    vtreefs::vtreefs_init(hooks, NR_INODES as u32, root_stat);

    // Count PID files and update tables (stub).
    OK
}

/// ProcFS entry point.
///
/// Initializes the tree, sets up root directory properties, and starts
/// the VTreeFS event loop (which does not return).
pub fn procfs_main() {
    let r = init_tree();
    if r != OK {
        return;
    }

    // Start VTreeFS (does not return).
    start_vtreefs();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_tree_returns_ok() {
        assert_eq!(init_tree(), OK);
    }

    #[test]
    fn init_hook_no_panic() {
        init_tree(); // initialises VTreeFS and fires init_hook
        init_hook(); // second call should also be fine
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
